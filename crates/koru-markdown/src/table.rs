// SPDX-License-Identifier: MPL-2.0

use std::borrow::Cow;

use parley::Layout;
use parley::layout::{Alignment as TextAlignment, AlignmentOptions};
use parley::style::{FontFamily, FontStyle, FontWeight, LineHeight, OverflowWrap, StyleProperty};
use vello::Scene;
use vello::kurbo::Affine;

use koru_blocks::table::{Alignment, ParsedTable, SyntaxError, TableCell, parse};
use koru_core::Mark;
use koru_draw::scene::{draw_layout, fill, rrect, stroke};
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Brush};

use crate::{LinkHit, LinkSpan, Styles};

pub const CELL_PAD_X: f32 = 10.0;
pub const CELL_PAD_Y: f32 = 7.0;
pub const MIN_COLUMN_W: f32 = 56.0;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Sizing {
    Fit,
    Natural,
}

pub struct TableGrid {
    pub parsed: ParsedTable,
    pub columns: Vec<f32>,
    pub rows: Vec<Vec<Layout<Brush>>>,
    /// Each cell's links, beside [`rows`](Self::rows): ranges in that cell's
    /// laid-out text, which is the cell's [`TableCell::text`].
    pub links: Vec<Vec<Vec<LinkSpan>>>,
    pub row_heights: Vec<f32>,
    pub width: f32,
    pub viewport_width: f32,
    pub height: f32,
}

pub fn layout(
    engine: &mut TextEngine,
    source: &str,
    available: f32,
    sizing: Sizing,
    styles: &Styles,
) -> Result<TableGrid, Vec<SyntaxError>> {
    let parsed = parse(source)?;
    let count = parsed.columns().max(1);
    let links = std::iter::once(&parsed.header)
        .chain(parsed.rows.iter())
        .map(|row| {
            (0..count)
                .map(|column| {
                    row.get(column)
                        .map(|cell| cell_links(cell, styles))
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut preferred = vec![MIN_COLUMN_W; count];
    for (row_index, row) in std::iter::once(&parsed.header)
        .chain(parsed.rows.iter())
        .enumerate()
    {
        for (column, cell) in row.iter().enumerate() {
            let laid = layout_cell(
                engine,
                cell,
                &links[row_index][column],
                styles,
                row_index == 0,
                None,
                TextAlignment::Start,
            );
            preferred[column] = preferred[column].max(laid.width() + CELL_PAD_X * 2.0);
        }
    }

    let viewport_width = available.max(MIN_COLUMN_W);
    let columns = match sizing {
        Sizing::Natural => preferred,
        Sizing::Fit => compact_widths(&preferred, viewport_width),
    };
    let width = columns.iter().sum();
    let mut rows = Vec::new();
    let mut row_heights = Vec::new();
    for (row_index, row) in std::iter::once(&parsed.header)
        .chain(parsed.rows.iter())
        .enumerate()
    {
        let mut layouts = Vec::with_capacity(count);
        let mut height = styles.metrics().line_height + CELL_PAD_Y * 2.0;
        for column in 0..count {
            let alignment = match parsed.alignments[column] {
                Alignment::Start => TextAlignment::Start,
                Alignment::Center => TextAlignment::Center,
                Alignment::End => TextAlignment::End,
            };
            let laid = layout_cell(
                engine,
                &row[column],
                &links[row_index][column],
                styles,
                row_index == 0,
                Some((columns[column] - CELL_PAD_X * 2.0).max(20.0)),
                alignment,
            );
            height = height.max(laid.height() + CELL_PAD_Y * 2.0);
            layouts.push(laid);
        }
        rows.push(layouts);
        row_heights.push(height);
    }
    let height = row_heights.iter().sum();
    Ok(TableGrid {
        parsed,
        columns,
        rows,
        links,
        row_heights,
        width,
        viewport_width,
        height,
    })
}

/// A cell's links: each `[…](…)` and `<…>` as one span however many marks
/// it crosses, and — when the surface asked ([`Styles::with_bare_links`]) —
/// each address written out bare outside them and outside code.
fn cell_links(cell: &TableCell, styles: &Styles) -> Vec<LinkSpan> {
    let mut links: Vec<LinkSpan> = Vec::new();
    for span in &cell.spans {
        let Some(url) = &span.link else {
            continue;
        };
        match links.last_mut() {
            Some(last) if last.url == *url && last.range.end == span.range.start => {
                last.range.end = span.range.end;
            }
            _ => links.push(LinkSpan {
                range: span.range.clone(),
                url: url.clone(),
            }),
        }
    }
    if styles.bare_links {
        let taken = links
            .iter()
            .map(|link| link.range.clone())
            .chain(
                cell.spans
                    .iter()
                    .filter(|span| span.marks.has(Mark::Code))
                    .map(|span| span.range.clone()),
            )
            .collect::<Vec<_>>();
        links.extend(crate::bare_links(&cell.text, &taken));
        links.sort_by_key(|link| link.range.start);
    }
    links
}

fn layout_cell(
    engine: &mut TextEngine,
    cell: &TableCell,
    links: &[LinkSpan],
    styles: &Styles,
    header: bool,
    width: Option<f32>,
    alignment: TextAlignment,
) -> Layout<Brush> {
    let metrics = styles.metrics();
    let weight = if header { 700.0 } else { styles.weight() };
    let mut builder = engine
        .layout_cx
        .ranged_builder(&mut engine.font_cx, &cell.text, 1.0, true);
    builder.push_default(StyleProperty::FontFamily(FontFamily::Source(
        Cow::Borrowed(styles.stack()),
    )));
    builder.push_default(StyleProperty::FontSize(metrics.size));
    builder.push_default(StyleProperty::LineHeight(LineHeight::Absolute(
        metrics.line_height,
    )));
    builder.push_default(StyleProperty::OverflowWrap(OverflowWrap::Anywhere));
    koru_draw::text::push_axes(&mut builder, metrics.size, weight);
    builder.push_default(StyleProperty::Brush(Brush(theme::p().text)));
    for span in &cell.spans {
        let range = span.range.clone();
        if span.marks.has(Mark::Bold) {
            builder.push(StyleProperty::FontWeight(FontWeight::BOLD), range.clone());
        }
        if span.marks.has(Mark::Italic) {
            builder.push(StyleProperty::FontStyle(FontStyle::Italic), range.clone());
        }
        if span.marks.has(Mark::Strike) {
            builder.push(StyleProperty::Strikethrough(true), range.clone());
            builder.push(
                StyleProperty::StrikethroughBrush(Some(Brush(theme::p().text))),
                range.clone(),
            );
        }
        if span.marks.has(Mark::Code) {
            builder.push(
                StyleProperty::FontFamily(FontFamily::Source(Cow::Borrowed(theme::code_stack()))),
                range.clone(),
            );
        }
    }
    for link in links {
        let range = link.range.clone();
        builder.push(StyleProperty::Underline(true), range.clone());
        builder.push(
            StyleProperty::UnderlineBrush(Some(Brush(theme::p().accent))),
            range.clone(),
        );
        builder.push(StyleProperty::Brush(Brush(theme::p().accent)), range);
    }
    let mut layout = builder.build(&cell.text);
    layout.break_all_lines(width);
    layout.align(alignment, AlignmentOptions::default());
    layout
}

pub fn compact_widths(preferred: &[f32], available: f32) -> Vec<f32> {
    let total: f32 = preferred.iter().sum();
    if total <= available {
        let spare = (available - total) / preferred.len().max(1) as f32;
        return preferred.iter().map(|width| width + spare).collect();
    }
    let floor_total = MIN_COLUMN_W * preferred.len() as f32;
    if floor_total >= available {
        return vec![available / preferred.len().max(1) as f32; preferred.len()];
    }
    let flexible = total - floor_total;
    preferred
        .iter()
        .map(|width| MIN_COLUMN_W + (width - MIN_COLUMN_W) / flexible * (available - floor_total))
        .collect()
}

pub fn cell_at(
    grid: &TableGrid,
    origin: (f32, f32),
    x: f32,
    y: f32,
    scroll_x: f32,
) -> Option<(usize, usize)> {
    let mut top = origin.1;
    let row = grid
        .row_heights
        .iter()
        .enumerate()
        .find_map(|(index, height)| {
            let hit = y >= top && y < top + *height;
            top += *height;
            hit.then_some(index)
        })?;
    let mut left = origin.0 - scroll_x;
    let column = grid.columns.iter().enumerate().find_map(|(index, width)| {
        let hit = x >= left && x < left + *width;
        left += *width;
        hit.then_some(index)
    })?;
    Some((row, column))
}

/// **The link under `(x, y)` in a table**, measured from where the grid's
/// first cell is drawn (a caller that scrolls the grid sideways adds its
/// scroll to `x`). Answered by the same [`crate::link_at`] a paragraph's
/// text is, inside the cell's text, so a link in a cell is found, lit and
/// followed exactly as one in a sentence.
pub fn link_at(grid: &TableGrid, x: f32, y: f32) -> Option<LinkHit<'_>> {
    let (row, column) = cell_at(grid, (0.0, 0.0), x, y, 0.0)?;
    let cell = if row == 0 {
        grid.parsed.header.get(column)?
    } else {
        grid.parsed.rows.get(row - 1)?.get(column)?
    };
    let layout = grid.rows.get(row)?.get(column)?;
    let links = grid.links.get(row)?.get(column)?;
    let left = grid.columns[..column].iter().sum::<f32>() + CELL_PAD_X;
    let top = grid.row_heights[..row].iter().sum::<f32>() + CELL_PAD_Y;
    let hit = crate::link_at(&cell.text, layout, links, x - left, y - top)?;
    Some(LinkHit {
        origin: (left, top),
        cell: Some((row, column)),
        ..hit
    })
}

pub trait CellOverlay {
    fn background(
        &self,
        _scene: &mut Scene,
        _root: Affine,
        _cell: vello::kurbo::Rect,
        _row: usize,
        _col: usize,
    ) {
    }
    fn under_text(
        &self,
        _scene: &mut Scene,
        _root: Affine,
        _cell: vello::kurbo::Rect,
        _row: usize,
        _col: usize,
        _layout: &Layout<Brush>,
    ) {
    }
}

pub fn draw_grid(
    scene: &mut Scene,
    root: Affine,
    grid: &TableGrid,
    x: f32,
    top: f32,
    scroll_x: f32,
    overlay: Option<&dyn CellOverlay>,
) {
    let mut y = top;
    for (row_index, row) in grid.rows.iter().enumerate() {
        let height = grid.row_heights[row_index];
        let mut cell_x = x - scroll_x;
        for (column, layout) in row.iter().enumerate() {
            let width = grid.columns[column];
            let cell = rrect(cell_x, y, width, height, 0.0);
            let rect = vello::kurbo::Rect::new(
                cell_x as f64,
                y as f64,
                (cell_x + width) as f64,
                (y + height) as f64,
            );
            if row_index == 0 {
                fill(scene, root, theme::p().code_bg, &cell);
            }
            if let Some(overlay) = overlay {
                overlay.background(scene, root, rect, row_index, column);
            }
            stroke(scene, root, theme::p().panel_border, 1.0, &cell);
            if let Some(overlay) = overlay {
                overlay.under_text(scene, root, rect, row_index, column, layout);
            }
            draw_layout(
                scene,
                root,
                layout,
                cell_x + CELL_PAD_X,
                y + CELL_PAD_Y,
                None,
            );
            cell_x += width;
        }
        y += height;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parley::layout::PositionedLayoutItem;
    use std::ops::Range;

    fn first_glyph_x(layout: &Layout<Brush>) -> f32 {
        layout
            .lines()
            .flat_map(|line| line.items())
            .find_map(|item| match item {
                PositionedLayoutItem::GlyphRun(run) => run.positioned_glyphs().next().map(|g| g.x),
                _ => None,
            })
            .unwrap()
    }

    fn first_weight(layout: &Layout<Brush>) -> f32 {
        layout
            .lines()
            .flat_map(|line| line.items())
            .find_map(|item| match item {
                PositionedLayoutItem::GlyphRun(run) => Some(run.run().font_attrs().weight.value()),
                _ => None,
            })
            .unwrap()
    }

    /// The middle of the first line of `range` in `grid`'s cell, in the
    /// grid's space.
    fn point_in(grid: &TableGrid, row: usize, column: usize, range: Range<usize>) -> (f32, f32) {
        use parley::editing::{Cursor, Selection};
        use parley::layout::Affinity;
        let layout = &grid.rows[row][column];
        let selection = Selection::new(
            Cursor::from_byte_index(layout, range.start, Affinity::Downstream),
            Cursor::from_byte_index(layout, range.end, Affinity::Upstream),
        );
        let mut first = None;
        selection.geometry_with(layout, |bb, _| {
            first.get_or_insert(bb);
        });
        let bb = first.expect("the range is drawn");
        let left = grid.columns[..column].iter().sum::<f32>() + CELL_PAD_X;
        let top = grid.row_heights[..row].iter().sum::<f32>() + CELL_PAD_Y;
        (
            left + ((bb.x0 + bb.x1) / 2.0) as f32,
            top + ((bb.y0 + bb.y1) / 2.0) as f32,
        )
    }

    /// **A link in a table's cell is a link** (koru-ai D95): found under the
    /// pointer with its address, its title and its cell — in the header and
    /// in the body, inside bold, and written bare when the surface asks.
    #[test]
    fn a_link_in_any_cell_is_found_under_the_pointer() {
        let source = "| [Guide](https://koru.test/h) | Video |\n| --- | --- |\n| Intro | [**Walk**through](https://youtu.test/1) |\n| Bare | see https://koru.test/b. |";
        let mut engine = TextEngine::new();
        let styles = Styles::ui().with_bare_links();
        let grid = layout(&mut engine, source, 600.0, Sizing::Fit, &styles).unwrap();

        let walk = grid.links[1][1].clone();
        assert_eq!(walk.len(), 1, "one link however many marks it crosses");
        let (x, y) = point_in(&grid, 1, 1, walk[0].range.clone());
        let hit = link_at(&grid, x, y).expect("the body cell's link");
        assert_eq!(hit.url, "https://youtu.test/1");
        assert_eq!(hit.title, "Walkthrough");
        assert_eq!(hit.cell, Some((1, 1)));

        let head = grid.links[0][0][0].range.clone();
        let (x, y) = point_in(&grid, 0, 0, head);
        assert_eq!(link_at(&grid, x, y).unwrap().url, "https://koru.test/h");

        let bare = grid.links[2][1][0].range.clone();
        let (x, y) = point_in(&grid, 2, 1, bare);
        let hit = link_at(&grid, x, y).unwrap();
        assert_eq!(
            (hit.url, hit.title),
            ("https://koru.test/b", "https://koru.test/b")
        );

        // Plain text in a cell, and the space between cells, is no link.
        let (x, y) = point_in(&grid, 1, 0, 0..5);
        assert!(link_at(&grid, x, y).is_none());
        assert!(link_at(&grid, -4.0, 4.0).is_none());
        // Without asking, a bare address stays text.
        let plain = layout(&mut engine, source, 600.0, Sizing::Fit, &Styles::ui()).unwrap();
        assert!(plain.links[2][1].is_empty());
    }

    #[test]
    fn compact_columns_fill_the_available_width() {
        let widths = compact_widths(&[100.0, 300.0], 240.0);
        assert!((widths.iter().sum::<f32>() - 240.0).abs() < 0.01);
        assert!(widths[1] > widths[0]);
    }

    #[test]
    fn compact_cells_wrap_an_unbroken_value_inside_its_column() {
        let mut engine = TextEngine::new();
        let grid = layout(
            &mut engine,
            "| App | State |\n| --- | --- |\n| an-uninterrupted-product-name | yes |",
            120.0,
            Sizing::Fit,
            &Styles::document(),
        )
        .unwrap();
        assert!(grid.rows[1][0].lines().count() > 1);
    }

    #[test]
    fn natural_keeps_preferred_width_while_fit_uses_the_viewport() {
        let source = "| Product | State |\n| --- | --- |\n| a very long product name that should stay natural | yes |";
        let mut engine = TextEngine::new();
        let fit = layout(&mut engine, source, 160.0, Sizing::Fit, &Styles::ui()).unwrap();
        let natural = layout(&mut engine, source, 160.0, Sizing::Natural, &Styles::ui()).unwrap();
        assert!(natural.width > fit.width);
    }

    #[test]
    fn delimiter_alignment_and_header_weight_reach_the_shaped_cells() {
        let source = "| Same | Same | Same |\n| :--- | :---: | ---: |\n| x | x | x |";
        let mut engine = TextEngine::new();
        let grid = layout(&mut engine, source, 480.0, Sizing::Fit, &Styles::ui()).unwrap();
        let xs = grid.rows[1].iter().map(first_glyph_x).collect::<Vec<_>>();
        assert!(xs[0] < xs[1] && xs[1] < xs[2]);
        // Normalized variation coordinates are empty for a static fallback
        // face; requested attributes remain meaningful for every shaped run.
        assert_eq!(first_weight(&grid.rows[0][0]), 700.0);
        assert_eq!(first_weight(&grid.rows[1][0]), Styles::ui().weight());
    }
}
