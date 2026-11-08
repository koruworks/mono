// SPDX-License-Identifier: MPL-2.0

//! Shared scrollbar treatment. Hierarchy scrolls rows; wrapped details scroll pixels.
#[cfg(test)]
mod tests;
use super::*;
use koru_ui::scrollbar::ScrollbarGeom;

#[derive(Default)]
pub(super) struct DetailsCache {
    lines: Vec<String>,
    selected: Option<usize>,
    hits: Vec<(Rect, super::details::Hit)>,
    width: f32,
    stack: &'static str,
    color: Option<vello::peniko::Color>,
    scene: Scene,
    height: f32,
}

impl Inspector {
    fn scroll_limit(&self) -> usize {
        if self.show_hierarchy {
            let visible = (self.control_rect(4).h / ROW).floor().max(1.0) as usize;
            self.hierarchy_rows().len().saturating_sub(visible)
        } else {
            (self.details_cache.height - self.control_rect(5).h)
                .max(0.0)
                .ceil() as usize
        }
    }

    pub(super) fn clamp_scroll(&mut self) {
        let max = self.scroll_limit();
        if self.show_hierarchy {
            self.hierarchy_scroll = self.hierarchy_scroll.min(max);
        } else {
            self.scroll = self.scroll.min(max);
        }
    }

    pub(super) fn scrollbar(&self) -> Option<ScrollbarGeom> {
        let area = self.control_rect(if self.show_hierarchy { 4 } else { 5 });
        let offset = if self.show_hierarchy {
            self.hierarchy_scroll
        } else {
            self.scroll
        };
        let unit = if self.show_hierarchy { ROW } else { 1.0 };
        ScrollbarGeom::for_content(
            area,
            area.h + self.scroll_limit() as f32 * unit,
            offset as f32 * unit,
        )
    }

    pub(super) fn scroll_to_pointer(&mut self, grab: f32) {
        if let Some(bar) = self.scrollbar() {
            let unit = if self.show_hierarchy { ROW } else { 1.0 };
            let offset = (bar.scroll_for_pointer(self.pointer.1, grab) / unit).round() as usize;
            if self.show_hierarchy {
                self.hierarchy_scroll = offset;
            } else {
                self.scroll = offset;
            }
            self.clamp_scroll();
        }
    }

    pub(super) fn draw_scrollbar(&self, canvas: &mut Scene, root: Affine) {
        if let Some(bar) = self.scrollbar() {
            koru_ui::scrollbar::draw(canvas, root, bar, true);
        }
    }

    /// The Details link under the pointer: an ancestor, or the notes' fold.
    pub(super) fn details_hit(&self) -> Option<super::details::Hit> {
        let area = self.control_rect(5);
        if !area.contains(self.pointer.0, self.pointer.1) {
            return None;
        }
        let (x, y) = (
            self.pointer.0 - area.x,
            self.pointer.1 - area.y + self.scroll as f32,
        );
        self.details_cache
            .hits
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, hit)| *hit)
    }

    pub(super) fn draw_details(&mut self, canvas: &mut Scene, root: Affine) {
        let area = self.control_rect(5);
        let width = (area.w - theme::SCROLLBAR_GUTTER_W).max(1.0);
        let color = theme::p().text;
        let cache = &mut self.details_cache;
        if cache.lines != self.lines
            || cache.selected != self.selected
            || cache.width != width
            || cache.color != Some(color)
            || cache.stack != diagnostic_stack()
        {
            let laid = super::details::lay_out(
                self.engine.get_or_insert_with(TextEngine::new),
                &self.details_lines,
                width,
            );
            cache.scene = laid.scene;
            cache.height = laid.height;
            cache.hits = laid.hits;
            cache.lines.clone_from(&self.lines);
            cache.selected = self.selected;
            cache.width = width;
            cache.color = Some(color);
            cache.stack = diagnostic_stack();
        }
        self.clamp_scroll();
        canvas.push_clip_layer(
            vello::peniko::Fill::NonZero,
            root,
            &scene::rect(area.x, area.y, area.w, area.h),
        );
        canvas.append(
            &self.details_cache.scene,
            Some(root * Affine::translate((area.x as f64, area.y as f64 - self.scroll as f64))),
        );
        canvas.pop_layer();
    }
}
