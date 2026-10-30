// SPDX-License-Identifier: MPL-2.0

use std::borrow::Cow;

use koru_draw::{
    scene::{draw_layout, fill, stroke},
    text::TextEngine,
    theme::Brush,
};
use koru_session_protocol::{CellWidth, TerminalCell, TerminalRow};
use parley::style::{
    FontFamily, FontFeatures, FontStyle, FontVariations, FontWeight, LineHeight, StyleProperty,
};
use parley::{
    Layout,
    layout::{Alignment, AlignmentOptions},
    setting::{FontFeature, FontVariation, Tag},
};
use vello::{
    Scene,
    kurbo::{Affine, BezPath, Rect},
};

use crate::{PaintRecord, ResolvedCellStyle, StyleEpoch, TerminalFace, TerminalStyle};

const GRID_FEATURES: [FontFeature; 5] = [
    FontFeature::new(Tag::new(b"liga"), 0),
    FontFeature::new(Tag::new(b"clig"), 0),
    FontFeature::new(Tag::new(b"dlig"), 0),
    FontFeature::new(Tag::new(b"calt"), 0),
    FontFeature::new(Tag::new(b"kern"), 0),
];

pub(crate) struct CachedRow {
    pub source: TerminalRow,
    pub epoch: StyleEpoch,
    backgrounds: Vec<PaintedBackground>,
    pub text: Scene,
    horizontal_rules: Vec<PaintedHorizontalRule>,
    vertical_rules: Vec<PaintedVerticalRule>,
    corners: Vec<PaintedCorner>,
    junctions: Vec<PaintedJunction>,
    block_elements: Vec<PaintedBlockElement>,
    pub records: Vec<PaintRecord>,
    pub style_runs: u64,
    pub private_use_cells: u64,
    pub replacement_cells: u64,
    pub box_drawing_cells: u64,
}

pub(crate) fn build_row(
    engine: &mut TextEngine,
    source: TerminalRow,
    style: TerminalStyle,
    columns: u16,
) -> CachedRow {
    let private_use_cells = source
        .cells
        .iter()
        .filter(|cell| cell.text.chars().any(is_private_use))
        .count() as u64;
    let replacement_cells = source
        .cells
        .iter()
        .filter(|cell| cell.text.contains('\u{fffd}'))
        .count() as u64;
    let box_drawing_cells = source
        .cells
        .iter()
        .filter(|cell| {
            cell.text
                .chars()
                .any(|character| ('\u{2500}'..='\u{259f}').contains(&character))
        })
        .count() as u64;
    let runs = group_runs(&source.cells, style, columns);
    let mut backgrounds = Vec::with_capacity(runs.len());
    let mut text_scene = Scene::new();
    let mut records = Vec::with_capacity(runs.len() * 2);
    let mut horizontal_rules = Vec::new();
    let mut vertical_rules = Vec::new();
    let mut corners = Vec::new();
    let mut junctions = Vec::new();
    let mut block_elements = Vec::new();
    let ground = style.palette().background;
    for run in &runs {
        let x = f32::from(run.first_column) * style.cell_width;
        // A run whose background *is* the ground paints nothing: the view
        // already filled its bounds with that colour, so an ordinary cell has
        // no rect of its own and therefore no edge to tear along.
        if run.style.background != ground {
            backgrounds.push(PaintedBackground {
                first_column: run.first_column,
                columns: run.columns,
                color: run.style.background,
            });
        }
        records.push(PaintRecord::Background {
            row: source.index.0,
            first_column: run.first_column,
            columns: run.columns,
            color: rgba8(run.style.background),
        });
        if !run.text.is_empty() {
            let layout = layout_run(engine, &run.text, run.style, style);
            // Center isolated fallback symbols (`•`, `⏺`, `❯`, `✻`, `↓`) inside
            // their cell span using optical ink centering so symbols with
            // asymmetric sidebearings or proportional fallback advances sit
            // squarely in the middle of their terminal cell. Private-use
            // glyphs (Powerline separators) keep their cell-edge origin.
            let slot_width = f32::from(run.columns) * style.cell_width;
            let glyph_x = if !run.mergeable_primary_grid
                && !run.text.chars().any(is_private_use)
                && run.text.chars().any(|c| !c.is_whitespace())
            {
                koru_draw::scene::centre_in(
                    &layout,
                    koru_draw::geometry::Rect::new(x, 0.0, slot_width, style.cell_height),
                )
                .0
            } else {
                x
            };
            draw_layout(
                &mut text_scene,
                Affine::IDENTITY,
                &layout,
                glyph_x,
                0.0,
                Some(run.style.foreground),
            );
            horizontal_rules.extend(run.horizontal_rules.iter().map(|rule| {
                PaintedHorizontalRule {
                    rule: *rule,
                    color: run.style.foreground,
                }
            }));
            vertical_rules.extend(run.vertical_rules.iter().map(|rule| PaintedVerticalRule {
                rule: *rule,
                color: run.style.foreground,
            }));
            corners.extend(run.corners.iter().map(|corner| PaintedCorner {
                corner: *corner,
                color: run.style.foreground,
            }));
            junctions.extend(run.junctions.iter().map(|junction| PaintedJunction {
                junction: *junction,
                color: run.style.foreground,
            }));
            block_elements.extend(
                run.block_elements
                    .iter()
                    .map(|element| PaintedBlockElement {
                        element: *element,
                        color: run.style.foreground,
                    }),
            );
            records.push(PaintRecord::Text {
                row: source.index.0,
                first_column: run.first_column,
                columns: run.columns,
                text: run.source_text.clone(),
                face: run.style.face,
                foreground: rgba8(run.style.foreground),
            });
        }
    }
    CachedRow {
        source,
        epoch: style.epoch,
        backgrounds,
        text: text_scene,
        horizontal_rules,
        vertical_rules,
        corners,
        junctions,
        block_elements,
        records,
        style_runs: runs.len() as u64,
        private_use_cells,
        replacement_cells,
        box_drawing_cells,
    }
}

/// **The grid every terminal rect has to land on: device pixels, absolute.**
///
/// A snap is only ever a claim about where something lands *on the glass*, so
/// it has to be computed in the coordinates of the glass. Two things stand
/// between a number in this module and that:
///
/// - **A scale that is not the DPI.** The shell draws a window under
///   `Affine::scale(zoom)` since display zoom became a transform, and
///   `koru-window` appends the whole frame under the DPI scale. The factor is
///   `scale × root`, and snapping to the DPI alone is right only while the zoom
///   is exactly 1.
/// - **A translation, which is where the grid actually is.** The shell hands a
///   surface `root * translate(region.x, region.y)` and a rect that starts at
///   the *local* origin, so the pane's width and the caption's height live in
///   the transform, not in the bounds. Snapping in local space and then being
///   translated by a fractional device offset moves every edge by the same
///   amount — which is exactly a uniform hairline at every row boundary rather
///   than a scattering, and exactly what the owner photographed. The caption is
///   36 logical pixels: a whole number of device pixels at every DPI Windows
///   offers, and `56.25` at 125 % DPI under a 125 % zoom.
///
/// So an axis carries both: how many device pixels a logical unit is worth, and
/// where logical zero lands. Both are read out of the transform rather than
/// passed in beside it, because read that way they cannot disagree with it —
/// whatever the frame is drawn under is what the rects are snapped to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DeviceGrid {
    x: Axis,
    y: Axis,
}

/// One axis of that grid: `device = value * pixels + origin`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Axis {
    pixels: f64,
    origin: f64,
}

impl Axis {
    /// A degenerate transform would turn every snap into a divide by zero or a
    /// NaN, so a scale that is not a real number is not one this can honour.
    fn new(pixels: f64, origin: f64) -> Self {
        Self {
            pixels: if pixels.is_normal() { pixels } else { 1.0 },
            origin: if origin.is_finite() { origin } else { 0.0 },
        }
    }

    /// Where a logical value lands, in device pixels.
    fn device(self, value: f64) -> f64 {
        value * self.pixels + self.origin
    }

    /// The logical value that lands on a device coordinate — `device`'s inverse,
    /// which is what carries a decision made in device space back out.
    fn logical(self, device: f64) -> f64 {
        (device - self.origin) / self.pixels
    }

    fn snap(self, value: f64) -> f64 {
        self.logical(self.device(value).round())
    }

    /// Device pixels per logical unit, as a magnitude — for thicknesses, which
    /// have no direction.
    fn magnitude(self) -> f64 {
        self.pixels.abs()
    }
}

impl DeviceGrid {
    pub(crate) fn new(root: Affine, scale: f32) -> Self {
        let coefficients = root.as_coeffs();
        let scale = f64::from(scale);
        Self {
            x: Axis::new(coefficients[0] * scale, coefficients[4] * scale),
            y: Axis::new(coefficients[3] * scale, coefficients[5] * scale),
        }
    }

    pub(crate) fn snap_x(self, value: f64) -> f64 {
        self.x.snap(value)
    }

    pub(crate) fn snap_y(self, value: f64) -> f64 {
        self.y.snap(value)
    }
}

/// One style run's cell background, kept as a column span rather than a rect.
///
/// **Why not a rect, and why not a cached `Scene`.** A row's backgrounds used
/// to be filled once into a row-local `Scene` at `x = column * cell_width` and
/// appended under a per-row translation. Cell metrics are measured advances, so
/// those edges land wherever the font puts them — mid-pixel, almost always —
/// and two abutting mid-pixel fills do not add up to one: each is antialiased
/// against what is already there, so the shared edge keeps a fraction of the
/// surface *underneath* both. Holding the span and snapping it at draw time,
/// where the real origin and the real transform are known, is the only way the
/// edges can be device-pixel exact.
#[derive(Clone, Copy)]
struct PaintedBackground {
    first_column: u16,
    columns: u16,
    color: peniko::Color,
}

#[derive(Clone)]
struct StyleRun {
    first_column: u16,
    columns: u16,
    text: String,
    source_text: String,
    style: ResolvedCellStyle,
    mergeable_primary_grid: bool,
    horizontal_rules: Vec<HorizontalRule>,
    vertical_rules: Vec<VerticalRule>,
    corners: Vec<BoxCorner>,
    junctions: Vec<BoxJunction>,
    block_elements: Vec<BlockElement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HorizontalRuleWeight {
    Light,
    Heavy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HorizontalRule {
    first_column: u16,
    columns: u16,
    weight: HorizontalRuleWeight,
}

#[derive(Clone, Copy)]
struct PaintedHorizontalRule {
    rule: HorizontalRule,
    color: peniko::Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VerticalRule {
    column: u16,
    weight: HorizontalRuleWeight,
}

#[derive(Clone, Copy)]
struct PaintedVerticalRule {
    rule: VerticalRule,
    color: peniko::Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CornerDirection {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BoxCorner {
    column: u16,
    direction: CornerDirection,
    horizontal_weight: HorizontalRuleWeight,
    vertical_weight: HorizontalRuleWeight,
    rounded: bool,
}

#[derive(Clone, Copy)]
struct PaintedCorner {
    corner: BoxCorner,
    color: peniko::Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BoxJunction {
    column: u16,
    left: Option<HorizontalRuleWeight>,
    up: Option<HorizontalRuleWeight>,
    right: Option<HorizontalRuleWeight>,
    down: Option<HorizontalRuleWeight>,
}

#[derive(Clone, Copy)]
struct PaintedJunction {
    junction: BoxJunction,
    color: peniko::Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockElementKind {
    Upper(u8),
    Lower(u8),
    Left(u8),
    Right(u8),
    Quadrants(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BlockElement {
    column: u16,
    kind: BlockElementKind,
}

#[derive(Clone, Copy)]
struct PaintedBlockElement {
    element: BlockElement,
    color: peniko::Color,
}

fn group_runs(cells: &[TerminalCell], terminal: TerminalStyle, columns: u16) -> Vec<StyleRun> {
    let mut runs: Vec<StyleRun> = Vec::new();
    for cell in cells {
        if cell.width == CellWidth::Zero || cell.column.0 >= columns {
            continue;
        }
        let width = u16::from(cell.width.columns()).min(columns - cell.column.0);
        if width == 0 {
            continue;
        }
        let resolved = terminal.resolve(cell.style);
        // Preserve private-use glyphs and isolate them to their protocol cell.
        // A selected/system fallback may provide Nerd Font or Powerline
        // coverage; replacing them here made known-good CLI icons impossible.
        let corner = box_corner(cell.text.as_str(), cell.column.0);
        let junction = box_junction(cell.text.as_str(), cell.column.0);
        let block_element = block_element(cell.text.as_str(), cell.column.0);
        let (text, horizontal_rule, vertical_rule) = match cell.text.as_str() {
            "\u{2500}" => (" ".to_owned(), Some(HorizontalRuleWeight::Light), None),
            "\u{2501}" => (" ".to_owned(), Some(HorizontalRuleWeight::Heavy), None),
            "\u{2502}" => (" ".to_owned(), None, Some(HorizontalRuleWeight::Light)),
            "\u{2503}" => (" ".to_owned(), None, Some(HorizontalRuleWeight::Heavy)),
            _ if corner.is_some() => (" ".to_owned(), None, None),
            _ if junction.is_some() => (" ".to_owned(), None, None),
            _ if block_element.is_some() => (" ".to_owned(), None, None),
            _ => (cell.text.clone(), None, None),
        };
        let mergeable_primary_grid = cell.width == CellWidth::One
            && text.chars().all(|character| {
                character.is_ascii() || ('\u{2500}'..='\u{259f}').contains(&character)
            });
        if let Some(last) = runs.last_mut()
            && last.style == resolved
            && last.first_column.saturating_add(last.columns) == cell.column.0
            && last.mergeable_primary_grid
            && mergeable_primary_grid
        {
            if let Some(weight) = horizontal_rule {
                push_horizontal_rule(&mut last.horizontal_rules, cell.column.0, width, weight);
            }
            if let Some(weight) = vertical_rule {
                last.vertical_rules.push(VerticalRule {
                    column: cell.column.0,
                    weight,
                });
            }
            if let Some(corner) = corner {
                last.corners.push(corner);
            }
            if let Some(junction) = junction {
                last.junctions.push(junction);
            }
            if let Some(block_element) = block_element {
                last.block_elements.push(block_element);
            }
            last.columns = last.columns.saturating_add(width);
            last.text.push_str(&text);
            last.source_text.push_str(&cell.text);
        } else {
            let mut horizontal_rules = Vec::new();
            if let Some(weight) = horizontal_rule {
                push_horizontal_rule(&mut horizontal_rules, cell.column.0, width, weight);
            }
            let vertical_rules = vertical_rule
                .map(|weight| VerticalRule {
                    column: cell.column.0,
                    weight,
                })
                .into_iter()
                .collect();
            let corners = corner.into_iter().collect();
            let junctions = junction.into_iter().collect();
            let block_elements = block_element.into_iter().collect();
            runs.push(StyleRun {
                first_column: cell.column.0,
                columns: width,
                text,
                source_text: cell.text.clone(),
                style: resolved,
                mergeable_primary_grid,
                horizontal_rules,
                vertical_rules,
                corners,
                junctions,
                block_elements,
            });
        }
    }
    runs
}

fn box_corner(text: &str, column: u16) -> Option<BoxCorner> {
    use CornerDirection::{BottomLeft, BottomRight, TopLeft, TopRight};
    use HorizontalRuleWeight::{Heavy, Light};
    let (direction, horizontal_weight, vertical_weight, rounded) = match text {
        "\u{250c}" => (TopLeft, Light, Light, false),
        "\u{250d}" => (TopLeft, Heavy, Light, false),
        "\u{250e}" => (TopLeft, Light, Heavy, false),
        "\u{250f}" => (TopLeft, Heavy, Heavy, false),
        "\u{2510}" => (TopRight, Light, Light, false),
        "\u{2511}" => (TopRight, Heavy, Light, false),
        "\u{2512}" => (TopRight, Light, Heavy, false),
        "\u{2513}" => (TopRight, Heavy, Heavy, false),
        "\u{2514}" => (BottomLeft, Light, Light, false),
        "\u{2515}" => (BottomLeft, Heavy, Light, false),
        "\u{2516}" => (BottomLeft, Light, Heavy, false),
        "\u{2517}" => (BottomLeft, Heavy, Heavy, false),
        "\u{2518}" => (BottomRight, Light, Light, false),
        "\u{2519}" => (BottomRight, Heavy, Light, false),
        "\u{251a}" => (BottomRight, Light, Heavy, false),
        "\u{251b}" => (BottomRight, Heavy, Heavy, false),
        "\u{256d}" => (TopLeft, Light, Light, true),
        "\u{256e}" => (TopRight, Light, Light, true),
        "\u{256f}" => (BottomRight, Light, Light, true),
        "\u{2570}" => (BottomLeft, Light, Light, true),
        _ => return None,
    };
    Some(BoxCorner {
        column,
        direction,
        horizontal_weight,
        vertical_weight,
        rounded,
    })
}

fn box_junction(text: &str, column: u16) -> Option<BoxJunction> {
    use HorizontalRuleWeight::{Heavy as H, Light as L};
    let (left, up, right, down) = match text {
        // Left T-junctions (U+251C..=U+2523)
        "\u{251c}" => (None, Some(L), Some(L), Some(L)),
        "\u{251d}" => (None, Some(L), Some(H), Some(L)),
        "\u{251e}" => (None, Some(H), Some(L), Some(L)),
        "\u{251f}" => (None, Some(L), Some(L), Some(H)),
        "\u{2520}" => (None, Some(H), Some(L), Some(H)),
        "\u{2521}" => (None, Some(H), Some(H), Some(L)),
        "\u{2522}" => (None, Some(L), Some(H), Some(H)),
        "\u{2523}" => (None, Some(H), Some(H), Some(H)),
        // Right T-junctions (U+2524..=U+252B)
        "\u{2524}" => (Some(L), Some(L), None, Some(L)),
        "\u{2525}" => (Some(H), Some(L), None, Some(L)),
        "\u{2526}" => (Some(L), Some(H), None, Some(L)),
        "\u{2527}" => (Some(L), Some(L), None, Some(H)),
        "\u{2528}" => (Some(L), Some(H), None, Some(H)),
        "\u{2529}" => (Some(H), Some(H), None, Some(L)),
        "\u{252a}" => (Some(H), Some(L), None, Some(H)),
        "\u{252b}" => (Some(H), Some(H), None, Some(H)),
        // Top T-junctions (U+252C..=U+2533)
        "\u{252c}" => (Some(L), None, Some(L), Some(L)),
        "\u{252d}" => (Some(H), None, Some(L), Some(L)),
        "\u{252e}" => (Some(L), None, Some(H), Some(L)),
        "\u{252f}" => (Some(H), None, Some(H), Some(L)),
        "\u{2530}" => (Some(L), None, Some(L), Some(H)),
        "\u{2531}" => (Some(H), None, Some(L), Some(H)),
        "\u{2532}" => (Some(L), None, Some(H), Some(H)),
        "\u{2533}" => (Some(H), None, Some(H), Some(H)),
        // Bottom T-junctions (U+2534..=U+253B)
        "\u{2534}" => (Some(L), Some(L), Some(L), None),
        "\u{2535}" => (Some(H), Some(L), Some(L), None),
        "\u{2536}" => (Some(L), Some(L), Some(H), None),
        "\u{2537}" => (Some(H), Some(L), Some(H), None),
        "\u{2538}" => (Some(L), Some(H), Some(L), None),
        "\u{2539}" => (Some(H), Some(H), Some(L), None),
        "\u{253a}" => (Some(L), Some(H), Some(H), None),
        "\u{253b}" => (Some(H), Some(H), Some(H), None),
        // Crosses (U+253C..=U+254B)
        "\u{253c}" => (Some(L), Some(L), Some(L), Some(L)),
        "\u{253d}" => (Some(H), Some(L), Some(L), Some(L)),
        "\u{253e}" => (Some(L), Some(L), Some(H), Some(L)),
        "\u{253f}" => (Some(H), Some(L), Some(H), Some(L)),
        "\u{2540}" => (Some(L), Some(H), Some(L), Some(L)),
        "\u{2541}" => (Some(L), Some(L), Some(L), Some(H)),
        "\u{2542}" => (Some(L), Some(H), Some(L), Some(H)),
        "\u{2543}" => (Some(H), Some(H), Some(L), Some(L)),
        "\u{2544}" => (Some(L), Some(H), Some(H), Some(L)),
        "\u{2545}" => (Some(H), Some(L), Some(L), Some(H)),
        "\u{2546}" => (Some(L), Some(L), Some(H), Some(H)),
        "\u{2547}" => (Some(H), Some(H), Some(H), Some(L)),
        "\u{2548}" => (Some(H), Some(L), Some(H), Some(H)),
        "\u{2549}" => (Some(H), Some(H), Some(L), Some(H)),
        "\u{254a}" => (Some(L), Some(H), Some(H), Some(H)),
        "\u{254b}" => (Some(H), Some(H), Some(H), Some(H)),
        // Half-lines and mixed-weight lines (U+2574..=U+257F)
        "\u{2574}" => (Some(L), None, None, None),
        "\u{2575}" => (None, Some(L), None, None),
        "\u{2576}" => (None, None, Some(L), None),
        "\u{2577}" => (None, None, None, Some(L)),
        "\u{2578}" => (Some(H), None, None, None),
        "\u{2579}" => (None, Some(H), None, None),
        "\u{257a}" => (None, None, Some(H), None),
        "\u{257b}" => (None, None, None, Some(H)),
        "\u{257c}" => (Some(L), None, Some(H), None),
        "\u{257d}" => (None, Some(L), None, Some(H)),
        "\u{257e}" => (Some(H), None, Some(L), None),
        "\u{257f}" => (None, Some(H), None, Some(L)),
        _ => return None,
    };
    Some(BoxJunction {
        column,
        left,
        up,
        right,
        down,
    })
}

fn block_element(text: &str, column: u16) -> Option<BlockElement> {
    use BlockElementKind::{Left, Lower, Quadrants, Right, Upper};
    let kind = match text {
        "\u{2580}" => Upper(4),
        "\u{2581}" => Lower(1),
        "\u{2582}" => Lower(2),
        "\u{2583}" => Lower(3),
        "\u{2584}" => Lower(4),
        "\u{2585}" => Lower(5),
        "\u{2586}" => Lower(6),
        "\u{2587}" => Lower(7),
        "\u{2588}" => Lower(8),
        "\u{2589}" => Left(7),
        "\u{258a}" => Left(6),
        "\u{258b}" => Left(5),
        "\u{258c}" => Left(4),
        "\u{258d}" => Left(3),
        "\u{258e}" => Left(2),
        "\u{258f}" => Left(1),
        "\u{2590}" => Right(4),
        // U+2591..U+2593 are patterned shades and intentionally stay glyphs.
        "\u{2594}" => Upper(1),
        "\u{2595}" => Right(1),
        "\u{2596}" => Quadrants(0b0100),
        "\u{2597}" => Quadrants(0b1000),
        "\u{2598}" => Quadrants(0b0001),
        "\u{2599}" => Quadrants(0b1101),
        "\u{259a}" => Quadrants(0b1001),
        "\u{259b}" => Quadrants(0b0111),
        "\u{259c}" => Quadrants(0b1011),
        "\u{259d}" => Quadrants(0b0010),
        "\u{259e}" => Quadrants(0b0110),
        "\u{259f}" => Quadrants(0b1110),
        _ => return None,
    };
    Some(BlockElement { column, kind })
}

/// Paint one row's non-default cell backgrounds on the device-pixel grid.
///
/// Called with the same origin and the same grid as [`draw_terminal_shapes`],
/// so a cell's background and the vector glyph drawn over it are snapped by one
/// rule from one pair of coordinates.
pub(crate) fn draw_row_backgrounds(
    scene: &mut Scene,
    root: Affine,
    grid: DeviceGrid,
    row: &CachedRow,
    style: TerminalStyle,
    origin_x: f64,
    origin_y: f64,
) {
    for painted in &row.backgrounds {
        fill(
            scene,
            root,
            painted.color,
            &snapped_span_rect(
                painted.first_column,
                painted.columns,
                style,
                grid,
                origin_x,
                origin_y,
            ),
        );
    }
}

pub(crate) fn draw_terminal_shapes(
    scene: &mut Scene,
    root: Affine,
    grid: DeviceGrid,
    row: &CachedRow,
    style: TerminalStyle,
    origin_x: f64,
    origin_y: f64,
) {
    for painted in &row.horizontal_rules {
        fill(
            scene,
            root,
            painted.color,
            &horizontal_rule_rect(painted.rule, style, grid, origin_x, origin_y),
        );
    }
    for painted in &row.vertical_rules {
        fill(
            scene,
            root,
            painted.color,
            &vertical_rule_rect(painted.rule, style, grid, origin_x, origin_y),
        );
    }
    for painted in &row.corners {
        draw_corner(scene, root, *painted, style, grid, origin_x, origin_y);
    }
    for painted in &row.junctions {
        draw_junction(scene, root, *painted, style, grid, origin_x, origin_y);
    }
    for painted in &row.block_elements {
        draw_block_element(scene, root, *painted, style, grid, origin_x, origin_y);
    }
}

fn draw_block_element(
    scene: &mut Scene,
    root: Affine,
    painted: PaintedBlockElement,
    style: TerminalStyle,
    grid: DeviceGrid,
    origin_x: f64,
    origin_y: f64,
) {
    use BlockElementKind::{Left, Lower, Quadrants, Right, Upper};
    let cell = snapped_cell_rect(painted.element.column, style, grid, origin_x, origin_y);
    match painted.element.kind {
        Upper(eighths) => fill(
            scene,
            root,
            painted.color,
            &Rect::new(cell.x0, cell.y0, cell.x1, eighth_y(cell, eighths, grid)),
        ),
        Lower(eighths) => fill(
            scene,
            root,
            painted.color,
            &Rect::new(cell.x0, eighth_y(cell, 8 - eighths, grid), cell.x1, cell.y1),
        ),
        Left(eighths) => fill(
            scene,
            root,
            painted.color,
            &Rect::new(cell.x0, cell.y0, eighth_x(cell, eighths, grid), cell.y1),
        ),
        Right(eighths) => fill(
            scene,
            root,
            painted.color,
            &Rect::new(eighth_x(cell, 8 - eighths, grid), cell.y0, cell.x1, cell.y1),
        ),
        Quadrants(mask) => {
            let center_x = eighth_x(cell, 4, grid);
            let center_y = eighth_y(cell, 4, grid);
            for (bit, quadrant) in [
                (0b0001, Rect::new(cell.x0, cell.y0, center_x, center_y)),
                (0b0010, Rect::new(center_x, cell.y0, cell.x1, center_y)),
                (0b0100, Rect::new(cell.x0, center_y, center_x, cell.y1)),
                (0b1000, Rect::new(center_x, center_y, cell.x1, cell.y1)),
            ] {
                if mask & bit != 0 {
                    fill(scene, root, painted.color, &quadrant);
                }
            }
        }
    }
}

fn snapped_cell_rect(
    column: u16,
    style: TerminalStyle,
    grid: DeviceGrid,
    origin_x: f64,
    origin_y: f64,
) -> Rect {
    snapped_span_rect(column, 1, style, grid, origin_x, origin_y)
}

/// The device-pixel rect of `columns` cells starting at `first_column`.
///
/// Every edge is one `round` of one expression, so a span that ends where the
/// next begins produces **one shared edge** rather than two that nearly meet,
/// and the row below shares this row's bottom for the same reason. That is the
/// whole of the tiling guarantee: neighbouring fills must not leave a fraction
/// of what is underneath showing between them.
fn snapped_span_rect(
    first_column: u16,
    columns: u16,
    style: TerminalStyle,
    grid: DeviceGrid,
    origin_x: f64,
    origin_y: f64,
) -> Rect {
    let x0 = grid.snap_x(origin_x + f64::from(first_column) * f64::from(style.cell_width));
    let x1 = grid.snap_x(
        origin_x + f64::from(first_column.saturating_add(columns)) * f64::from(style.cell_width),
    );
    let y0 = grid.snap_y(origin_y);
    let y1 = grid.snap_y(origin_y + f64::from(style.cell_height));
    Rect::new(x0, y0, x1, y1)
}

fn eighth_x(cell: Rect, eighths: u8, grid: DeviceGrid) -> f64 {
    grid.snap_x(cell.x0 + (cell.x1 - cell.x0) * f64::from(eighths) / 8.0)
}

fn eighth_y(cell: Rect, eighths: u8, grid: DeviceGrid) -> f64 {
    grid.snap_y(cell.y0 + (cell.y1 - cell.y0) * f64::from(eighths) / 8.0)
}

fn draw_corner(
    scene: &mut Scene,
    root: Affine,
    painted: PaintedCorner,
    style: TerminalStyle,
    grid: DeviceGrid,
    origin_x: f64,
    origin_y: f64,
) {
    let corner = painted.corner;
    let horizontal = horizontal_rule_rect(
        HorizontalRule {
            first_column: corner.column,
            columns: 1,
            weight: corner.horizontal_weight,
        },
        style,
        grid,
        origin_x,
        origin_y,
    );
    let vertical = vertical_rule_rect(
        VerticalRule {
            column: corner.column,
            weight: corner.vertical_weight,
        },
        style,
        grid,
        origin_x,
        origin_y,
    );
    if corner.rounded {
        draw_rounded_corner(
            scene,
            root,
            painted.color,
            corner,
            horizontal,
            vertical,
            style,
        );
        return;
    }

    let (horizontal, vertical) = square_corner_rects(corner.direction, horizontal, vertical);
    fill(scene, root, painted.color, &horizontal);
    fill(scene, root, painted.color, &vertical);
}

fn draw_junction(
    scene: &mut Scene,
    root: Affine,
    painted: PaintedJunction,
    style: TerminalStyle,
    grid: DeviceGrid,
    origin_x: f64,
    origin_y: f64,
) {
    use HorizontalRuleWeight::{Heavy, Light};
    let junction = painted.junction;
    let center_vertical_weight = if junction.up == Some(Heavy) || junction.down == Some(Heavy) {
        Heavy
    } else {
        Light
    };
    let center_horizontal_weight = if junction.left == Some(Heavy) || junction.right == Some(Heavy)
    {
        Heavy
    } else {
        Light
    };
    let center_vertical = vertical_rule_rect(
        VerticalRule {
            column: junction.column,
            weight: center_vertical_weight,
        },
        style,
        grid,
        origin_x,
        origin_y,
    );
    let center_horizontal = horizontal_rule_rect(
        HorizontalRule {
            first_column: junction.column,
            columns: 1,
            weight: center_horizontal_weight,
        },
        style,
        grid,
        origin_x,
        origin_y,
    );

    if junction.left == junction.right {
        if let Some(weight) = junction.left {
            let full = horizontal_rule_rect(
                HorizontalRule {
                    first_column: junction.column,
                    columns: 1,
                    weight,
                },
                style,
                grid,
                origin_x,
                origin_y,
            );
            fill(scene, root, painted.color, &full);
        }
    } else {
        if let Some(weight) = junction.left {
            let arm = horizontal_rule_rect(
                HorizontalRule {
                    first_column: junction.column,
                    columns: 1,
                    weight,
                },
                style,
                grid,
                origin_x,
                origin_y,
            );
            fill(
                scene,
                root,
                painted.color,
                &Rect::new(arm.x0, arm.y0, center_vertical.x1, arm.y1),
            );
        }
        if let Some(weight) = junction.right {
            let arm = horizontal_rule_rect(
                HorizontalRule {
                    first_column: junction.column,
                    columns: 1,
                    weight,
                },
                style,
                grid,
                origin_x,
                origin_y,
            );
            fill(
                scene,
                root,
                painted.color,
                &Rect::new(center_vertical.x0, arm.y0, arm.x1, arm.y1),
            );
        }
    }

    if junction.up == junction.down {
        if let Some(weight) = junction.up {
            let full = vertical_rule_rect(
                VerticalRule {
                    column: junction.column,
                    weight,
                },
                style,
                grid,
                origin_x,
                origin_y,
            );
            fill(scene, root, painted.color, &full);
        }
    } else {
        if let Some(weight) = junction.up {
            let arm = vertical_rule_rect(
                VerticalRule {
                    column: junction.column,
                    weight,
                },
                style,
                grid,
                origin_x,
                origin_y,
            );
            fill(
                scene,
                root,
                painted.color,
                &Rect::new(arm.x0, arm.y0, arm.x1, center_horizontal.y1),
            );
        }
        if let Some(weight) = junction.down {
            let arm = vertical_rule_rect(
                VerticalRule {
                    column: junction.column,
                    weight,
                },
                style,
                grid,
                origin_x,
                origin_y,
            );
            fill(
                scene,
                root,
                painted.color,
                &Rect::new(arm.x0, center_horizontal.y0, arm.x1, arm.y1),
            );
        }
    }
}

fn square_corner_rects(
    direction: CornerDirection,
    horizontal: Rect,
    vertical: Rect,
) -> (Rect, Rect) {
    match direction {
        CornerDirection::TopLeft => (
            Rect::new(vertical.x0, horizontal.y0, horizontal.x1, horizontal.y1),
            Rect::new(vertical.x0, horizontal.y0, vertical.x1, vertical.y1),
        ),
        CornerDirection::TopRight => (
            Rect::new(horizontal.x0, horizontal.y0, vertical.x1, horizontal.y1),
            Rect::new(vertical.x0, horizontal.y0, vertical.x1, vertical.y1),
        ),
        CornerDirection::BottomLeft => (
            Rect::new(vertical.x0, horizontal.y0, horizontal.x1, horizontal.y1),
            Rect::new(vertical.x0, vertical.y0, vertical.x1, horizontal.y1),
        ),
        CornerDirection::BottomRight => (
            Rect::new(horizontal.x0, horizontal.y0, vertical.x1, horizontal.y1),
            Rect::new(vertical.x0, vertical.y0, vertical.x1, horizontal.y1),
        ),
    }
}

fn draw_rounded_corner(
    scene: &mut Scene,
    root: Affine,
    color: peniko::Color,
    corner: BoxCorner,
    horizontal: Rect,
    vertical: Rect,
    style: TerminalStyle,
) {
    let x = (vertical.x0 + vertical.x1) * 0.5;
    let y = (horizontal.y0 + horizontal.y1) * 0.5;
    let radius = (f64::from(style.cell_width).min(f64::from(style.cell_height)) * 0.28)
        .max(horizontal.height());
    let mut path = BezPath::new();
    match corner.direction {
        CornerDirection::TopLeft => {
            path.move_to((x, vertical.y1));
            path.line_to((x, y + radius));
            path.quad_to((x, y), (x + radius, y));
            path.line_to((horizontal.x1, y));
        }
        CornerDirection::TopRight => {
            path.move_to((horizontal.x0, y));
            path.line_to((x - radius, y));
            path.quad_to((x, y), (x, y + radius));
            path.line_to((x, vertical.y1));
        }
        CornerDirection::BottomLeft => {
            path.move_to((x, vertical.y0));
            path.line_to((x, y - radius));
            path.quad_to((x, y), (x + radius, y));
            path.line_to((horizontal.x1, y));
        }
        CornerDirection::BottomRight => {
            path.move_to((horizontal.x0, y));
            path.line_to((x - radius, y));
            path.quad_to((x, y), (x, y - radius));
            path.line_to((x, vertical.y0));
        }
    }
    stroke(scene, root, color, horizontal.height(), &path);
}

fn horizontal_rule_rect(
    rule: HorizontalRule,
    style: TerminalStyle,
    grid: DeviceGrid,
    origin_x: f64,
    origin_y: f64,
) -> vello::kurbo::Rect {
    let weight = match rule.weight {
        HorizontalRuleWeight::Light => 1.0,
        HorizontalRuleWeight::Heavy => 2.0,
    };
    let thickness_px = (weight * grid.y.magnitude()).round().max(1.0);
    let center_px = grid.y.device(origin_y + f64::from(style.cell_height) * 0.5);
    let top_px = (center_px - thickness_px * 0.5).floor();
    let y0 = grid.y.logical(top_px);
    let y1 = grid.y.logical(top_px + thickness_px);
    let x0 = grid.snap_x(origin_x + f64::from(rule.first_column) * f64::from(style.cell_width));
    let x1 = grid.snap_x(
        origin_x
            + f64::from(rule.first_column.saturating_add(rule.columns))
                * f64::from(style.cell_width),
    );
    vello::kurbo::Rect::new(x0, y0, x1, y1)
}

fn vertical_rule_rect(
    rule: VerticalRule,
    style: TerminalStyle,
    grid: DeviceGrid,
    origin_x: f64,
    origin_y: f64,
) -> vello::kurbo::Rect {
    let weight = match rule.weight {
        HorizontalRuleWeight::Light => 1.0,
        HorizontalRuleWeight::Heavy => 2.0,
    };
    let thickness_px = (weight * grid.x.magnitude()).round().max(1.0);
    let center_px = grid
        .x
        .device(origin_x + (f64::from(rule.column) + 0.5) * f64::from(style.cell_width));
    let left_px = (center_px - thickness_px * 0.5).floor();
    let x0 = grid.x.logical(left_px);
    let x1 = grid.x.logical(left_px + thickness_px);
    let y0 = grid.snap_y(origin_y);
    let y1 = grid.snap_y(origin_y + f64::from(style.cell_height));
    vello::kurbo::Rect::new(x0, y0, x1, y1)
}

fn push_horizontal_rule(
    rules: &mut Vec<HorizontalRule>,
    first_column: u16,
    columns: u16,
    weight: HorizontalRuleWeight,
) {
    if let Some(last) = rules.last_mut()
        && last.weight == weight
        && last.first_column.saturating_add(last.columns) == first_column
    {
        last.columns = last.columns.saturating_add(columns);
    } else {
        rules.push(HorizontalRule {
            first_column,
            columns,
            weight,
        });
    }
}

fn is_private_use(character: char) -> bool {
    matches!(
        u32::from(character),
        0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD
    )
}

fn layout_run(
    engine: &mut TextEngine,
    text: &str,
    resolved: ResolvedCellStyle,
    style: TerminalStyle,
) -> Layout<Brush> {
    let mut builder = engine
        .layout_cx
        .ranged_builder(&mut engine.font_cx, text, 1.0, true);
    builder.push_default(StyleProperty::FontFamily(FontFamily::Source(
        Cow::Borrowed(style.stack),
    )));
    builder.push_default(StyleProperty::FontSize(style.metrics.size));
    builder.push_default(StyleProperty::LineHeight(LineHeight::Absolute(
        style.metrics.line_height,
    )));
    builder.push_default(StyleProperty::FontFeatures(FontFeatures::List(
        Cow::Borrowed(&GRID_FEATURES),
    )));
    // Keep `FontWeight` at 400 / 700 so static fallback faces (`Consolas`,
    // `Menlo`, `Segoe UI Symbol`) match their Regular or Bold face without
    // triggering fontique's `embolden` faux-bold stroke on Regular, while
    // driving variable faces (`JetBrains Mono`, `Source Code Pro`,
    // `Cascadia Mono`) above 400 to compensate for vello's un-gamma-corrected,
    // un-stem-darkened area anti-aliasing. At 13–14px (100% zoom), 435 keeps
    // auto-hinted horizontal bars (`5`, `u`, `r`) to a single pixel row; larger
    // sizes ramp up to 500 where counters have room for two-pixel stems.
    let regular_axis_weight =
        (435.0 + ((style.metrics.size - 14.0) / 2.8).clamp(0.0, 1.0) * 65.0).round();
    let (style_weight, axis_weight) = match resolved.face {
        TerminalFace::Regular | TerminalFace::Italic => (400.0, regular_axis_weight),
        TerminalFace::Bold | TerminalFace::BoldItalic => (700.0, 700.0),
    };
    builder.push_default(StyleProperty::FontWeight(FontWeight::new(style_weight)));
    builder.push_default(StyleProperty::FontVariations(FontVariations::List(
        Cow::Owned(vec![
            FontVariation::new(Tag::new(b"opsz"), style.metrics.size),
            FontVariation::new(Tag::new(b"wght"), axis_weight),
        ]),
    )));
    if matches!(
        resolved.face,
        TerminalFace::Italic | TerminalFace::BoldItalic
    ) {
        builder.push_default(StyleProperty::FontStyle(FontStyle::Italic));
    }
    builder.push_default(StyleProperty::Brush(Brush(resolved.foreground)));
    if resolved.underline {
        builder.push_default(StyleProperty::Underline(true));
        builder.push_default(StyleProperty::UnderlineBrush(Some(Brush(
            resolved.foreground,
        ))));
    }
    if resolved.strikeout {
        builder.push_default(StyleProperty::Strikethrough(true));
        builder.push_default(StyleProperty::StrikethroughBrush(Some(Brush(
            resolved.foreground,
        ))));
    }
    let mut layout = builder.build(text);
    layout.break_all_lines(None);
    layout.align(Alignment::Start, AlignmentOptions::default());
    layout
}

fn rgba8(color: peniko::Color) -> [u8; 4] {
    color
        .components
        .map(|component| (component.clamp(0.0, 1.0) * 255.0).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_session_protocol::{CellStyle, ColumnIndex, RowIndex, TerminalColor};
    use parley::layout::PositionedLayoutItem;

    fn style(engine: &mut TextEngine) -> TerminalStyle {
        TerminalStyle::capture(engine, 1.0)
    }

    #[test]
    fn style_runs_group_cells_not_glyphs() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let cells = (0..120)
            .map(|column| TerminalCell {
                column: ColumnIndex(column),
                text: "x".into(),
                width: CellWidth::One,
                style: CellStyle::default(),
            })
            .collect::<Vec<_>>();
        assert_eq!(group_runs(&cells, terminal, 120).len(), 1);
    }

    #[test]
    fn continuation_cells_draw_nothing() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let cells = vec![
            TerminalCell {
                column: ColumnIndex(0),
                text: "界".into(),
                width: CellWidth::Two,
                style: CellStyle::default(),
            },
            TerminalCell {
                column: ColumnIndex(1),
                text: String::new(),
                width: CellWidth::Zero,
                style: CellStyle::default(),
            },
        ];
        let runs = group_runs(&cells, terminal, 2);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].columns, 2);
        assert_eq!(runs[0].text, "界");
    }

    #[test]
    fn private_use_is_preserved_in_its_protocol_cell() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let runs = group_runs(
            &[TerminalCell {
                column: ColumnIndex(0),
                text: "\u{e0b0}".into(),
                width: CellWidth::One,
                style: CellStyle::default(),
            }],
            terminal,
            2,
        );
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "\u{e0b0}");
        assert!(is_private_use('\u{e0b0}'));
    }

    #[test]
    fn continuous_box_glyphs_become_one_vector_span() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let cells = std::iter::repeat_n('\u{2500}', 120)
            .enumerate()
            .map(|(column, character)| TerminalCell {
                column: ColumnIndex(column as u16),
                text: character.to_string(),
                width: CellWidth::One,
                style: CellStyle::default(),
            })
            .collect::<Vec<_>>();
        let runs = group_runs(&cells, terminal, 120);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].columns, 120);
        assert_eq!(runs[0].text, " ".repeat(120));
        assert_eq!(runs[0].source_text, "\u{2500}".repeat(120));
        assert_eq!(
            runs[0].horizontal_rules,
            [HorizontalRule {
                first_column: 0,
                columns: 120,
                weight: HorizontalRuleWeight::Light,
            }]
        );
    }

    #[test]
    fn vector_rule_edges_land_on_device_pixels() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let grid = DeviceGrid::new(Affine::IDENTITY, 1.25);
        let line = horizontal_rule_rect(
            HorizontalRule {
                first_column: 2,
                columns: 10,
                weight: HorizontalRuleWeight::Light,
            },
            terminal,
            grid,
            7.3,
            3.7,
        );
        for edge in [line.x0, line.x1, line.y0, line.y1] {
            assert!(on_device_grid(edge, 1.25));
        }
        assert_eq!((line.height() * 1.25).round(), 1.0);

        let vertical = VerticalRule {
            column: 3,
            weight: HorizontalRuleWeight::Light,
        };
        let first = vertical_rule_rect(vertical, terminal, grid, 7.3, 3.7);
        let second = vertical_rule_rect(
            vertical,
            terminal,
            grid,
            7.3,
            3.7 + f64::from(terminal.cell_height),
        );
        assert!(on_device_grid(first.x0, 1.25));
        assert!(on_device_grid(first.x1, 1.25));
        assert_eq!((first.width() * 1.25).round(), 1.0);
        assert_eq!(first.y1, second.y0, "neighboring rows share one edge");
    }

    /// **The foreground path has the same exposure as the backgrounds, and is
    /// fixed at the same level.** A TUI's borders are rules, and a rule that
    /// lands off the device grid is a blurred two-pixel line rather than a
    /// crisp one — the same missed translation, showing as softness instead of
    /// as a seam. One `DeviceGrid` snaps both, so this asserts the whole chain
    /// on the line art as well.
    #[test]
    fn rules_and_block_fractions_stay_on_the_grid_under_a_translated_root() {
        let mut engine = TextEngine::new();
        let mut terminal = style(&mut engine);
        terminal.cell_width = 8.203_125;
        terminal.cell_height = 20.0;
        for scale in REAL_SCALES {
            for zoom in REAL_ZOOMS {
                for (pane, caption) in REAL_ORIGINS {
                    let root = Affine::scale(zoom) * Affine::translate((pane, caption));
                    let grid = DeviceGrid::new(root, scale);
                    let origin_x = f64::from(crate::CONTENT_INSET_X);
                    let where_ = format!("scale {scale} zoom {zoom} origin ({pane}, {caption})");

                    let line = horizontal_rule_rect(
                        HorizontalRule {
                            first_column: 2,
                            columns: 10,
                            weight: HorizontalRuleWeight::Light,
                        },
                        terminal,
                        grid,
                        origin_x,
                        0.0,
                    );
                    for edge in [line.x0, line.x1] {
                        assert!(
                            whole_pixel(device_x(edge, root, scale)),
                            "rule x at {where_}"
                        );
                    }
                    for edge in [line.y0, line.y1] {
                        assert!(
                            whole_pixel(device_y(edge, root, scale)),
                            "rule y at {where_}"
                        );
                    }

                    let bar = vertical_rule_rect(
                        VerticalRule {
                            column: 3,
                            weight: HorizontalRuleWeight::Heavy,
                        },
                        terminal,
                        grid,
                        origin_x,
                        0.0,
                    );
                    for edge in [bar.x0, bar.x1] {
                        assert!(
                            whole_pixel(device_x(edge, root, scale)),
                            "bar x at {where_}"
                        );
                    }

                    // A block glyph's own fill, and the eighths inside it.
                    let cell = snapped_cell_rect(5, terminal, grid, origin_x, 0.0);
                    for edge in [cell.x0, cell.x1, eighth_x(cell, 4, grid)] {
                        assert!(
                            whole_pixel(device_x(edge, root, scale)),
                            "block x at {where_}"
                        );
                    }
                    for edge in [cell.y0, cell.y1, eighth_y(cell, 3, grid)] {
                        assert!(
                            whole_pixel(device_y(edge, root, scale)),
                            "block y at {where_}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn rounded_and_square_corners_leave_the_font_path() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let cells = ["\u{2570}", "\u{250c}", "\u{2503}"]
            .into_iter()
            .enumerate()
            .map(|(column, text)| TerminalCell {
                column: ColumnIndex(column as u16),
                text: text.to_owned(),
                width: CellWidth::One,
                style: CellStyle::default(),
            })
            .collect::<Vec<_>>();
        let runs = group_runs(&cells, terminal, 3);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "   ");
        assert_eq!(runs[0].source_text, "\u{2570}\u{250c}\u{2503}");
        assert_eq!(runs[0].corners.len(), 2);
        assert_eq!(runs[0].corners[0].direction, CornerDirection::BottomLeft);
        assert!(runs[0].corners[0].rounded);
        assert_eq!(runs[0].corners[1].direction, CornerDirection::TopLeft);
        assert!(!runs[0].corners[1].rounded);
        assert_eq!(runs[0].vertical_rules.len(), 1);
        assert_eq!(
            runs[0].vertical_rules[0].weight,
            HorizontalRuleWeight::Heavy
        );
    }

    #[test]
    fn junctions_crosses_and_half_lines_leave_the_font_path() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let glyphs: Vec<String> = ('\u{251c}'..='\u{254b}')
            .chain('\u{2574}'..='\u{257f}')
            .map(|c| c.to_string())
            .collect();
        let cells = glyphs
            .iter()
            .enumerate()
            .map(|(column, text)| TerminalCell {
                column: ColumnIndex(column as u16),
                text: text.clone(),
                width: CellWidth::One,
                style: CellStyle::default(),
            })
            .collect::<Vec<_>>();
        let count = cells.len();
        let runs = group_runs(&cells, terminal, count as u16);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, " ".repeat(count));
        assert_eq!(runs[0].junctions.len(), count);
        assert_eq!(
            runs[0].junctions[0],
            BoxJunction {
                column: 0,
                left: None,
                up: Some(HorizontalRuleWeight::Light),
                right: Some(HorizontalRuleWeight::Light),
                down: Some(HorizontalRuleWeight::Light),
            }
        );
    }

    #[test]
    fn solid_block_elements_leave_the_font_path_but_shades_do_not() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let cells = [
            "\u{2580}", "\u{2584}", "\u{2588}", "\u{258c}", "\u{2590}", "\u{259a}", "\u{2592}",
        ]
        .into_iter()
        .enumerate()
        .map(|(column, text)| TerminalCell {
            column: ColumnIndex(column as u16),
            text: text.to_owned(),
            width: CellWidth::One,
            style: CellStyle::default(),
        })
        .collect::<Vec<_>>();
        let runs = group_runs(&cells, terminal, cells.len() as u16);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "      \u{2592}");
        assert_eq!(runs[0].block_elements.len(), 6);
        assert_eq!(
            runs[0].block_elements[5].kind,
            BlockElementKind::Quadrants(0b1001)
        );
    }

    /// The real-world display scales this has to tile at. Windows offers 100,
    /// 125, 150 and 175 %, and the shell's display zoom multiplies whichever of
    /// them the monitor reports — so the interesting numbers are fractions, not
    /// the integers a test reaches for first.
    const REAL_SCALES: [f32; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];
    /// Display zooms the shell composes on top of the DPI scale.
    const REAL_ZOOMS: [f64; 6] = [1.0, 1.1, 1.25, 1.5, 0.9, 0.75];
    /// Surface origins the shell puts in the transform: the window's own zero,
    /// the shipped caption over a default pane, and panes the owner has dragged
    /// to somewhere between logical pixels. The grid is never at (0, 0) in a
    /// real window, and a harness that assumes it is proves nothing.
    const REAL_ORIGINS: [(f64, f64); 4] = [(0.0, 0.0), (232.0, 36.0), (231.3, 36.0), (187.5, 36.0)];

    /// Where a logical x lands under the whole chain, in device pixels: the
    /// root's own scale and translation, then the DPI append over the frame.
    fn device_x(edge: f64, root: Affine, scale: f32) -> f64 {
        let coefficients = root.as_coeffs();
        f64::from(scale) * (coefficients[0] * edge + coefficients[4])
    }

    fn device_y(edge: f64, root: Affine, scale: f32) -> f64 {
        let coefficients = root.as_coeffs();
        f64::from(scale) * (coefficients[3] * edge + coefficients[5])
    }

    fn whole_pixel(device: f64) -> bool {
        (device - device.round()).abs() < 1e-9
    }

    /// The no-translation form, for the axis-local checks.
    fn on_device_grid(edge: f64, device: f64) -> bool {
        whole_pixel(edge * device)
    }

    #[test]
    fn block_element_boundaries_land_on_device_pixels() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let grid = DeviceGrid::new(Affine::IDENTITY, 1.25);
        let cell = snapped_cell_rect(3, terminal, grid, 7.3, 3.7);
        for edge in [
            cell.x0,
            cell.x1,
            cell.y0,
            cell.y1,
            eighth_x(cell, 4, grid),
            eighth_y(cell, 4, grid),
        ] {
            assert!(on_device_grid(edge, 1.25));
        }
    }

    /// **The tiling proof, swept rather than sampled — and in absolute device
    /// space, because that is the only space a seam exists in.**
    ///
    /// A seam is one pair of edges that fail to meet, so a test that checks one
    /// pair proves nothing about a grid two hundred columns wide. Every column
    /// boundary of a wide grid and every row boundary of a tall one is asserted
    /// to be *one* number — no gap, no overlap — and to sit on a whole device
    /// pixel **after the surface's own translation**, at every scale, zoom and
    /// origin the product actually renders at.
    ///
    /// The translation is the part an earlier attempt left out. Snapping is
    /// exact in local coordinates and still lands mid-pixel on the glass if the
    /// grid's origin does, and every edge then misses by the *same* amount —
    /// which is a hairline across the whole grid rather than a speckle.
    #[test]
    fn spans_and_rows_tile_exactly_at_every_real_scale_zoom_and_origin() {
        let mut engine = TextEngine::new();
        let mut terminal = style(&mut engine);
        // A measured advance and a line height that land between device pixels,
        // which is the ordinary case rather than a contrived one.
        terminal.cell_width = 8.203_125;
        terminal.cell_height = 20.0;
        for scale in REAL_SCALES {
            for zoom in REAL_ZOOMS {
                for (pane, caption) in REAL_ORIGINS {
                    let root = Affine::scale(zoom) * Affine::translate((pane, caption));
                    let grid = DeviceGrid::new(root, scale);
                    let where_ = format!("scale {scale} zoom {zoom} origin ({pane}, {caption})");
                    // The terminal's own inset inside the surface it was given.
                    let origin_x = f64::from(crate::CONTENT_INSET_X);
                    let origin_y = 0.0;
                    for column in 0..200_u16 {
                        let left = snapped_span_rect(column, 1, terminal, grid, origin_x, origin_y);
                        let right =
                            snapped_span_rect(column + 1, 1, terminal, grid, origin_x, origin_y);
                        assert_eq!(
                            left.x1, right.x0,
                            "column {column} at {where_} leaves a seam"
                        );
                        assert!(
                            whole_pixel(device_x(left.x1, root, scale)),
                            "column {column} at {where_} is off the device grid"
                        );
                        // A run of several cells has to end where the cell after
                        // it begins, or a style change tears where plain text
                        // does not.
                        let run =
                            snapped_span_rect(0, column + 1, terminal, grid, origin_x, origin_y);
                        assert_eq!(run.x1, left.x1, "a run's end is its last cell's end");
                    }
                    for row in 0..80_u16 {
                        let y = origin_y + f64::from(row) * f64::from(terminal.cell_height);
                        let next = origin_y + f64::from(row + 1) * f64::from(terminal.cell_height);
                        let above = snapped_span_rect(0, 4, terminal, grid, origin_x, y);
                        let below = snapped_span_rect(0, 4, terminal, grid, origin_x, next);
                        assert_eq!(above.y1, below.y0, "row {row} at {where_} leaves a seam");
                        assert!(
                            whole_pixel(device_y(above.y1, root, scale)),
                            "row {row} at {where_} is off the device grid"
                        );
                    }
                }
            }
        }
    }

    /// **Both halves of the transform, and what each one costs when it is
    /// missed.** Two attempts at this bug died here, so the arithmetic is
    /// stated rather than left to the sweep.
    ///
    /// The scale is not the DPI: the shell's root carries the display zoom. And
    /// the root is not only a scale: it carries the surface's position, so a
    /// snap that is exact in local coordinates still lands mid-pixel on the
    /// glass whenever that position does.
    #[test]
    fn the_device_grid_follows_the_whole_root_transform() {
        let mut engine = TextEngine::new();
        let mut terminal = style(&mut engine);
        terminal.cell_width = 8.203_125;
        terminal.cell_height = 20.0;

        // Half one: the zoom is in the transform and not in `scale`.
        let dpi_only = DeviceGrid::new(Affine::IDENTITY, 1.5);
        let edge = snapped_span_rect(7, 3, terminal, dpi_only, 0.0, 0.0).x1;
        assert!(on_device_grid(edge, 1.5), "on the grid it was snapped to");
        assert!(
            !on_device_grid(edge, 1.5 * 1.25),
            "and off the grid a zoomed frame is drawn on"
        );

        // Half two: the caption is 36 logical pixels, which is 56.25 device
        // pixels at 125 % DPI under a 125 % zoom. A grid that reads only the
        // scale out of the transform snaps to a lattice the glass has moved.
        let root = Affine::scale(1.25) * Affine::translate((232.0, 36.0));
        let scale_only = DeviceGrid {
            x: Axis::new(1.25 * 1.25, 0.0),
            y: Axis::new(1.25 * 1.25, 0.0),
        };
        let edge = snapped_span_rect(0, 1, terminal, scale_only, 0.0, 0.0).y1;
        assert!(
            !whole_pixel(device_y(edge, root, 1.25)),
            "ignoring the translation puts every row edge off the device grid"
        );
        let grid = DeviceGrid::new(root, 1.25);
        let edge = snapped_span_rect(0, 1, terminal, grid, 0.0, 0.0).y1;
        assert!(whole_pixel(device_y(edge, root, 1.25)));
    }

    /// A degenerate transform must not turn a snap into a NaN.
    #[test]
    fn a_collapsed_transform_still_produces_a_finite_rect() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let grid = DeviceGrid::new(Affine::scale(0.0), 1.0);
        let rect = snapped_span_rect(3, 2, terminal, grid, 8.0, 0.0);
        assert!(rect.x0.is_finite() && rect.x1.is_finite());
        assert!(rect.y0.is_finite() && rect.y1.is_finite());
    }

    /// A block glyph's fill and the cell background under it are one rect.
    #[test]
    fn a_block_glyph_lands_exactly_on_its_own_cell_background() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let grid = DeviceGrid::new(Affine::scale(1.25), 1.5);
        assert_eq!(
            snapped_cell_rect(5, terminal, grid, 7.3, 3.7),
            snapped_span_rect(5, 1, terminal, grid, 7.3, 3.7)
        );
    }

    /// **The seam class this removes by construction.** An ordinary cell's
    /// background is the ground the view already painted, so it emits no rect —
    /// and a boundary that has no edge cannot leak whatever is under it, at any
    /// scale, on any grid, however the snapping is derived.
    #[test]
    fn default_background_runs_paint_nothing_of_their_own() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let plain = TerminalRow {
            index: RowIndex(0),
            cells: (0..40)
                .map(|column| TerminalCell {
                    column: ColumnIndex(column),
                    text: "x".into(),
                    width: CellWidth::One,
                    style: CellStyle::default(),
                })
                .collect(),
            wrapped_to_next: false,
        };
        let row = build_row(&mut engine, plain, terminal, 40);
        assert!(row.backgrounds.is_empty());
        // The record still describes the frame: what a cell's background *is*
        // has not changed, only whether anything had to be painted to show it.
        assert!(matches!(
            row.records.first(),
            Some(PaintRecord::Background { .. })
        ));

        let explicit = TerminalRow {
            index: RowIndex(0),
            cells: (0..40)
                .map(|column| TerminalCell {
                    column: ColumnIndex(column),
                    text: "x".into(),
                    width: CellWidth::One,
                    style: CellStyle {
                        background: TerminalColor::Rgb {
                            red: 0x0d,
                            green: 0x0d,
                            blue: 0x0d,
                        },
                        ..CellStyle::default()
                    },
                })
                .collect(),
            wrapped_to_next: false,
        };
        let row = build_row(&mut engine, explicit, terminal, 40);
        assert_eq!(row.backgrounds.len(), 1, "a TUI's own canvas still paints");
    }

    #[test]
    fn inverse_resolves_before_dim_and_bold_does_not_brighten() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let normal = terminal.resolve(CellStyle {
            foreground: TerminalColor::Indexed { index: 1 },
            bold: false,
            ..CellStyle::default()
        });
        let bold = terminal.resolve(CellStyle {
            foreground: TerminalColor::Indexed { index: 1 },
            bold: true,
            ..CellStyle::default()
        });
        assert_eq!(normal.foreground, bold.foreground);
        assert_eq!(bold.face, TerminalFace::Bold);
    }

    #[test]
    fn packaged_terminal_faces_are_real_and_keep_one_advance() {
        let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap()
            .join("assets/fonts");
        if !fonts.join("JetBrainsMono.ttf").is_file()
            || !fonts.join("JetBrainsMono-Italic.ttf").is_file()
        {
            eprintln!("skipping packaged-face proof: run koru.bat fonts");
            return;
        }
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let mut widths = Vec::new();
        for cell_style in [
            CellStyle::default(),
            CellStyle {
                bold: true,
                ..CellStyle::default()
            },
            CellStyle {
                italic: true,
                ..CellStyle::default()
            },
            CellStyle {
                bold: true,
                italic: true,
                ..CellStyle::default()
            },
        ] {
            let layout = layout_run(&mut engine, "M", terminal.resolve(cell_style), terminal);
            widths.push(layout.width());
            for line in layout.lines() {
                for item in line.items() {
                    if let PositionedLayoutItem::GlyphRun(run) = item {
                        assert!(!run.run().synthesis().embolden());
                        assert!(run.run().synthesis().skew().is_none());
                        assert!(
                            run.run()
                                .normalized_coords()
                                .iter()
                                .any(|&coord| coord != 0),
                            "variable terminal face must drive wght axis above default 400"
                        );
                    }
                }
            }
        }
        assert!(widths.iter().all(|width| (width - widths[0]).abs() < 0.01));
    }

    #[test]
    fn packaged_box_and_block_ranges_have_no_missing_glyphs() {
        let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap()
            .join("assets/fonts");
        if !fonts.join("JetBrainsMono.ttf").is_file() {
            return;
        }
        let text: String = ('\u{2500}'..='\u{259f}').collect();
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let layout = layout_run(
            &mut engine,
            &text,
            terminal.resolve(CellStyle::default()),
            terminal,
        );
        for line in layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    assert!(run.positioned_glyphs().all(|glyph| glyph.id != 0));
                }
            }
        }
    }

    #[test]
    fn terminal_media_symbol_uses_a_real_fallback_glyph() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let layout = layout_run(
            &mut engine,
            "\u{23f5}",
            terminal.resolve(CellStyle::default()),
            terminal,
        );
        let mut glyph_ids = Vec::new();
        for line in layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    assert!(
                        !run.run().synthesis().embolden(),
                        "static fallback glyphs must not faux-embolden in regular terminal runs"
                    );
                    glyph_ids.extend(run.positioned_glyphs().map(|glyph| glyph.id));
                }
            }
        }
        assert_eq!(glyph_ids.len(), 1);
        assert_ne!(glyph_ids[0], 0, "U+23F5 must not render as tofu");
    }

    #[test]
    fn isolated_symbols_are_optically_centered_in_their_cell_slot() {
        let mut engine = TextEngine::new();
        let terminal = style(&mut engine);
        let row = TerminalRow {
            index: RowIndex(0),
            cells: vec![TerminalCell {
                column: ColumnIndex(0),
                text: "↓".into(),
                width: CellWidth::One,
                style: CellStyle::default(),
            }],
            wrapped_to_next: false,
        };
        let cached = build_row(&mut engine, row, terminal, 80);
        assert_eq!(cached.records.len(), 2);
        assert!(matches!(
            &cached.records[1],
            PaintRecord::Text { text, .. } if text == "↓"
        ));
    }
}
