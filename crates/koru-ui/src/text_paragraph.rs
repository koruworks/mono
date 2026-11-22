// SPDX-License-Identifier: MPL-2.0

//! Chrome text as a component: a paragraph that wraps into a caller
//! rectangle, and its single-line companion.
//!
//! The paragraph is the `<div>` of chrome text: hand it any length of copy
//! and a rectangle and it wraps, draws, and reports the height it used. The
//! typography provenance is part of the contract — the inspector names the
//! role, the requested tier and the actual wrap, and each visual line lands
//! *inside* the paragraph's boundary rather than beside it.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{inspect, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;

/// A paragraph: any length of text wrapped into the caller's rectangle.
///
/// `role` is the typography provenance the inspector reports — name the call
/// that produced the style, e.g. `"theme::role::row()"`, the same convention
/// `typography::part` uses.
pub struct KoruTextParagraph<'a> {
    text: &'a str,
    style: theme::TextStyle,
    role: &'static str,
    color: Color,
    highlight: &'a str,
}

impl<'a> KoruTextParagraph<'a> {
    pub const fn new(
        text: &'a str,
        style: theme::TextStyle,
        role: &'static str,
        color: Color,
    ) -> Self {
        Self {
            text,
            style,
            role,
            color,
            highlight: "",
        }
    }

    pub fn highlight(mut self, query: &'a str) -> Self {
        self.highlight = query;
        self
    }

    /// The height the text takes wrapped to `width`, without drawing it.
    pub fn measure(&self, engine: &mut TextEngine, width: f32) -> f32 {
        engine
            .simple_layout_wrapped(self.text, self.style, width.max(1.0))
            .height()
    }

    /// Wrap to `rect.w` and draw from the rect's top-left. Returns the height
    /// used, so a caller stacking the next element measures nothing twice.
    #[track_caller]
    pub fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
    ) -> f32 {
        let layout = engine.simple_layout_wrapped(self.text, self.style, rect.w.max(1.0));
        let height = layout.height();
        let _paragraph = inspect::scope(
            "KoruTextParagraph",
            "component",
            root,
            scene::rect(rect.x, rect.y, rect.w, height),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::text_paragraph",
                    "shared implementation",
                );
                r.property("typography role", self.role, "declared by caller");
                r.property(
                    "requested family",
                    theme::ui_stack(),
                    "theme::ui_stack(); actual faces are on descendant text runs",
                );
                r.property(
                    "requested size / line height",
                    format!(
                        "{} / {}",
                        self.style.metrics.size, self.style.metrics.line_height
                    ),
                    self.role,
                );
                r.property("requested weight", self.style.weight.value(), self.role);
                r.property("wrap width", rect.w, "caller rectangle");
                r.property("measured height", height, "actual Parley layout");
                r.property("lines", layout.lines().count(), "actual Parley layout");
                r.property("colour", inspect::color(self.color), "caller paint");
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        highlight_layout(
            canvas,
            root,
            &layout,
            self.text,
            self.highlight,
            rect.x,
            rect.y,
        );
        scene::draw_layout(canvas, root, &layout, rect.x, rect.y, Some(self.color));
        height
    }
}

/// One line of chrome text with its typography provenance declared — the
/// paragraph's single-line companion, for labels that never wrap.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn draw_line(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    value: &str,
    x: f32,
    y: f32,
    style: theme::TextStyle,
    role: &'static str,
    color: Color,
) {
    let layout = engine.simple_layout(value, style);
    crate::typography::line("Text line", canvas, root, &layout, x, y, style, role, color);
}

/// Highlight text without changing its layout or baseline.
#[allow(clippy::too_many_arguments)]
pub fn highlight_layout(
    canvas: &mut Scene,
    root: Affine,
    layout: &parley::Layout<koru_draw::theme::Brush>,
    text: &str,
    query: &str,
    x: f32,
    y: f32,
) {
    if query.trim().is_empty() {
        return;
    }
    for range in highlight_ranges(text, query) {
        scene::fill_text_range(
            canvas,
            root,
            layout,
            range,
            x,
            y,
            Color::from_rgba8(255, 220, 0, if theme::is_dark() { 85 } else { 110 }),
        );
    }
}

#[cfg(test)]
mod tests;

/// Case-insensitive search ranges mapped back to original UTF-8 boundaries.
/// Lowercasing may expand a character, so folded byte offsets are never used
/// directly as offsets into the displayed string.
pub fn highlight_ranges(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    let mut folded = String::new();
    let mut boundaries = Vec::new();
    for (start, ch) in text.char_indices() {
        for lower in ch.to_lowercase() {
            for _ in 0..lower.len_utf8() {
                boundaries.push((start, start + ch.len_utf8()));
            }
            folded.push(lower);
        }
    }
    let mut ranges = Vec::new();
    for word in query.split_whitespace().map(str::to_lowercase) {
        for (start, found) in folded.match_indices(&word) {
            let end = start + found.len();
            if end > start {
                ranges.push(boundaries[start].0..boundaries[end - 1].1);
            }
        }
    }
    ranges.sort_by_key(|r| r.start);
    ranges.dedup();
    ranges
}
