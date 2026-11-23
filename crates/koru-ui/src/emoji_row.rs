// SPDX-License-Identifier: MPL-2.0

//! `KoruEmojiRow`: a row of emoji to choose one from with a single press,
//! wrapping onto as many lines as a width needs.
//!
//! Each cell is a square holding one emoji. The chosen one is ringed and the
//! keyboard's is ringed again outside that — **`KoruSwatch`'s rule**, painted
//! by the same crate-private ring helper (`Swatch::draw_rings`), so a swatch
//! and an emoji cell can never disagree about what chosen and focused look
//! like. `selected` is `None` when the value came from somewhere else (the
//! full picker) and is not one of the row's; then no cell is ringed.
//!
//! It has the wrapped shape `ColorPickerRow` has: `columns(width)` per line,
//! `height(width)` for the lines a width needs, each line's band from
//! `lines`, every cell from `cells`, the cell under a point from `hit`.
//! **The rings' reach is kept inside `height(width)`**: the cells sit a
//! ring's reach below the top, lines are twice the reach apart, and the
//! reach is kept again under the last line, so nothing the row paints spills
//! outside what it measured. Paint, pointer and a screen reader read this one
//! geometry.
//!
//! One inspector component, `KoruEmojiRow`, with a `KoruEmojiCell` part per
//! cell.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{inspect, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

use crate::swatch::Swatch;

/// How much of a cell its emoji fills.
const GLYPH_OF_CELL: f32 = 0.62;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmojiRow<'a> {
    emoji: &'a [&'a str],
    cell: f32,
    gap: f32,
    selected: Option<usize>,
    hovered: Option<usize>,
    focused: Option<usize>,
    alpha: f32,
}

impl<'a> EmojiRow<'a> {
    /// A cell's side unless the host sizes it.
    pub const CELL: f32 = 28.0;
    /// Between two cells on a line unless the host sizes it.
    pub const GAP: f32 = 8.0;

    pub fn new(emoji: &'a [&'a str]) -> Self {
        Self {
            emoji,
            cell: Self::CELL,
            gap: Self::GAP,
            selected: None,
            hovered: None,
            focused: None,
            alpha: 1.0,
        }
    }

    /// The host's cell side and the gap between cells on a line.
    pub fn cell(mut self, side: f32, gap: f32) -> Self {
        self.cell = side;
        self.gap = gap;
        self
    }

    pub fn selected(mut self, index: Option<usize>) -> Self {
        self.selected = index;
        self
    }

    pub fn hovered(mut self, index: Option<usize>) -> Self {
        self.hovered = index;
        self
    }

    pub fn focused(mut self, index: Option<usize>) -> Self {
        self.focused = index;
        self
    }

    /// Multiplies the rings: an inert row draws faint.
    pub fn alpha(mut self, alpha: f32) -> Self {
        self.alpha = alpha;
        self
    }

    /// The pitch from one line's top to the next: a cell, then the rings'
    /// reach below it and above the next.
    fn line_pitch(&self) -> f32 {
        self.cell + Swatch::RING_REACH * 2.0
    }

    /// How many cells fit on one line `width` wide; at least one.
    pub fn columns(&self, width: f32) -> usize {
        ((width + self.gap) / (self.cell + self.gap))
            .floor()
            .max(1.0) as usize
    }

    fn line_count(&self, width: f32) -> usize {
        self.emoji.len().div_ceil(self.columns(width)).max(1)
    }

    /// The width every cell needs on one line.
    pub fn width(&self) -> f32 {
        let count = self.emoji.len();
        if count == 0 {
            0.0
        } else {
            count as f32 * self.cell + (count - 1) as f32 * self.gap
        }
    }

    /// The height the row needs at `width`, the rings' reach above the first
    /// line and below the last included.
    pub fn height(&self, width: f32) -> f32 {
        let lines = self.line_count(width);
        Swatch::RING_REACH * 2.0 + self.cell + (lines - 1) as f32 * self.line_pitch()
    }

    /// Each line's first cell index and its band — `rect`'s width wide, a
    /// cell tall — in a row whose rect is `rect`.
    pub fn lines(&self, rect: Rect) -> Vec<(usize, Rect)> {
        let columns = self.columns(rect.w);
        (0..self.line_count(rect.w))
            .map(|line| {
                (
                    line * columns,
                    Rect::new(
                        rect.x,
                        rect.y + Swatch::RING_REACH + line as f32 * self.line_pitch(),
                        rect.w,
                        self.cell,
                    ),
                )
            })
            .collect()
    }

    /// Where every cell sits, in order, each on its line.
    pub fn cells(&self, rect: Rect) -> Vec<Rect> {
        let columns = self.columns(rect.w);
        let top = rect.y + Swatch::RING_REACH;
        (0..self.emoji.len())
            .map(|index| {
                let (line, column) = (index / columns, index % columns);
                Rect::new(
                    rect.x + column as f32 * (self.cell + self.gap),
                    top + line as f32 * self.line_pitch(),
                    self.cell,
                    self.cell,
                )
            })
            .collect()
    }

    /// Which cell a point is on, on any line.
    pub fn hit(&self, rect: Rect, x: f32, y: f32) -> Option<usize> {
        self.cells(rect).iter().position(|cell| cell.contains(x, y))
    }

    /// Draw the row in `rect`: one `KoruEmojiRow` component and a
    /// `KoruEmojiCell` part per cell, each emoji centred on its ink, the rings
    /// by `KoruSwatch`'s rule.
    #[track_caller]
    pub fn draw(&self, canvas: &mut Scene, root: Affine, engine: &mut TextEngine, rect: Rect) {
        let height = self.height(rect.w);
        let lines = self.line_count(rect.w);
        let _row = inspect::scope(
            "KoruEmojiRow",
            "component",
            root,
            scene::rect(rect.x, rect.y, rect.w, height),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::emoji_row::EmojiRow",
                    "shared implementation",
                );
                r.property("cells", self.emoji.len(), "caller emoji");
                r.property("lines", lines, "EmojiRow::columns(width)");
                r.property(
                    "selected",
                    self.selected.map_or("none".into(), |i| i.to_string()),
                    "caller state",
                );
                r.property(
                    "ring clearance",
                    Swatch::RING_REACH,
                    "Swatch::RING_REACH; kept inside the measured height",
                );
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        let glyph = self.cell * GLYPH_OF_CELL;
        let style = theme::TextStyle::new(
            theme::TextMetrics::new(glyph, glyph),
            theme::FontWeight::Medium,
        );
        for (index, (cell, emoji)) in self.cells(rect).into_iter().zip(self.emoji).enumerate() {
            let selected = self.selected == Some(index);
            let hovered = self.hovered == Some(index);
            let focused = self.focused == Some(index);
            let _cell = inspect::scope(
                "KoruEmojiCell",
                "part",
                root,
                scene::rect(cell.x, cell.y, cell.w, cell.h),
                |r| {
                    r.property("emoji", *emoji, "caller emoji");
                    r.property(
                        "selected / hovered / focused",
                        format!("{selected} / {hovered} / {focused}"),
                        "instance state",
                    );
                },
            );
            let layout = engine.simple_layout(emoji, style);
            // `None` for the run's color: a color emoji draws its own.
            let (x, y) = scene::centre_in(&layout, cell);
            scene::draw_layout(canvas, root, &layout, x, y, None);
            Swatch::draw_rings(
                canvas,
                root,
                cell,
                Swatch::RADIUS,
                selected,
                hovered,
                focused,
                self.alpha,
            );
        }
    }
}

#[cfg(test)]
mod tests;
