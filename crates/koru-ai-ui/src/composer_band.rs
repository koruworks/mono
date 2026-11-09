// SPDX-License-Identifier: MPL-2.0

//! Geometry for the composer's context band. The surface measures its live
//! chips once for each layout pass; paint, hit testing and wheel input read
//! these same rectangles. No editor reflow is needed for hover.

use koru_draw::geometry::Rect;

pub(crate) const GAP: f32 = 6.0;
pub(crate) const MAX_CHIP_W: f32 = 280.0;
pub(crate) const MAX_LINES: usize = 3;

#[derive(Clone, Debug, Default)]
pub(crate) struct ChipBand {
    pub(crate) rects: Vec<Rect>,
    pub(crate) shown_height: f32,
    pub(crate) content_height: f32,
    pub(crate) scroll: f32,
}

impl ChipBand {
    pub(crate) fn layout(widths: &[f32], available: f32, chip_height: f32, scroll: f32) -> Self {
        if widths.is_empty() {
            return Self::default();
        }
        let available = available.max(1.0);
        let mut rects = Vec::with_capacity(widths.len());
        let (mut x, mut y) = (0.0, 0.0);
        for width in widths {
            let width = width.min(available).max(1.0);
            if x > 0.0 && x + width > available {
                x = 0.0;
                y += chip_height + GAP;
            }
            rects.push(Rect::new(x, y, width, chip_height));
            x += width + GAP;
        }
        let content_height = y + chip_height;
        let shown_height =
            content_height.min(chip_height * MAX_LINES as f32 + GAP * (MAX_LINES - 1) as f32);
        Self {
            rects,
            shown_height,
            content_height,
            scroll: scroll.clamp(0.0, (content_height - shown_height).max(0.0)),
        }
    }

    pub(crate) fn max_scroll(&self) -> f32 {
        (self.content_height - self.shown_height).max(0.0)
    }

    pub(crate) fn band_rect(&self, x: f32, y: f32, width: f32) -> Rect {
        Rect::new(x, y, width, self.shown_height)
    }

    pub(crate) fn chip_rect(&self, index: usize, x: f32, y: f32) -> Option<Rect> {
        let rect = *self.rects.get(index)?;
        Some(Rect::new(
            x + rect.x,
            y + rect.y - self.scroll,
            rect.w,
            rect.h,
        ))
    }

    pub(crate) fn hit(
        &self,
        x: f32,
        y: f32,
        origin_x: f32,
        origin_y: f32,
        width: f32,
    ) -> Option<usize> {
        if !self.band_rect(origin_x, origin_y, width).contains(x, y) {
            return None;
        }
        (0..self.rects.len()).find(|index| {
            self.chip_rect(*index, origin_x, origin_y)
                .is_some_and(|rect| rect.contains(x, y))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_scrolls_after_three_lines_with_shared_hit_geometry() {
        let band = ChipBand::layout(&[70.0; 8], 150.0, 40.0, 0.0);
        assert_eq!(band.content_height, 4.0 * 40.0 + 3.0 * GAP);
        assert_eq!(band.shown_height, 3.0 * 40.0 + 2.0 * GAP);
        assert!(band.max_scroll() > 0.0);
        assert_eq!(band.hit(2.0, 2.0, 0.0, 0.0, 150.0), Some(0));
        assert_eq!(
            band.hit(2.0, band.shown_height + 1.0, 0.0, 0.0, 150.0),
            None
        );
        let scrolled = ChipBand::layout(&[70.0; 8], 150.0, 40.0, f32::MAX);
        assert_eq!(scrolled.scroll, scrolled.max_scroll());
        assert_eq!(
            scrolled.hit(2.0, scrolled.shown_height - 2.0, 0.0, 0.0, 150.0),
            Some(6)
        );
    }
}
