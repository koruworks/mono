// SPDX-License-Identifier: MPL-2.0

//! Declared chrome text provenance, scoped to text paint rather than its control.

use koru_draw::{geometry::Rect, inspect, scene, theme};
use parley::Layout;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;

/// The caller supplies the same role used to measure and paint this text.
/// Adjacent artwork and icons must be painted outside the returned scope.
#[track_caller]
pub(crate) fn part(
    name: &'static str,
    root: Affine,
    bounds: Rect,
    style: theme::TextStyle,
    role: &'static str,
) -> inspect::Scope {
    inspect::scope(
        name,
        "part",
        root,
        scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
        |record| {
            record.property("typography role", role, "declared by shared text paint");
            record.property(
                "requested family",
                theme::ui_stack(),
                "theme::ui_stack(); actual faces are on descendant text runs",
            );
            record.property(
                "requested size / line height",
                format!("{} / {}", style.metrics.size, style.metrics.line_height),
                role,
            );
            record.property("requested weight", style.weight.value(), role);
            record.property("docs", "docs/features/koruui/TYPOGRAPHY.md", "catalog");
        },
    )
}

/// Paint one visual line of chrome text, recorded **once**: the declared
/// typography provenance and the actual Parley line box are properties of the
/// same node, with the glyph runs as its children. Painting through [`part`]
/// plus a `scene::draw_layout*` call instead would nest a second, identically
/// named line node between this one and its runs — one visual line, one node.
///
/// A label that wraps is not this: it is a `text_paragraph::KoruTextParagraph`,
/// which keeps one child node per visual line because there are several.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub(crate) fn line(
    name: &'static str,
    canvas: &mut Scene,
    root: Affine,
    layout: &Layout<theme::Brush>,
    x: f32,
    y: f32,
    style: theme::TextStyle,
    role: &'static str,
    color: Color,
) {
    let Some(line) = layout.lines().next() else {
        return;
    };
    let lm = line.metrics();
    let _line = inspect::scope(
        name,
        "part",
        root,
        scene::rect(
            x + lm.offset,
            y + lm.baseline - lm.ascent - lm.leading * 0.5,
            lm.advance,
            lm.line_height,
        ),
        |record| {
            record.property("typography role", role, "declared by shared text paint");
            record.property(
                "requested family",
                theme::ui_stack(),
                "theme::ui_stack(); actual faces are on descendant text runs",
            );
            record.property(
                "requested size / line height",
                format!("{} / {}", style.metrics.size, style.metrics.line_height),
                role,
            );
            record.property("requested weight", style.weight.value(), role);
            record.property("line height", lm.line_height, "actual Parley line box");
            record.property("baseline", y + lm.baseline, "local layout coordinates");
            record.property("ink bounds", "Not exposed", "line box is not glyph ink");
            record.property("docs", "docs/features/koruui/TYPOGRAPHY.md", "catalog");
        },
    );
    scene::draw_line_items(canvas, root, &line, x, y, Some(color));
}

#[cfg(test)]
mod tests;

/// Stable optical baseline for short controls in the interface font. Using
/// one representative ascent and descent avoids moving the baseline when
/// a value changes from "Automatic" to "Detect automatically".
pub fn control_label_y(
    engine: &mut koru_draw::text::TextEngine,
    rect: koru_draw::geometry::Rect,
) -> f32 {
    let reference = engine.simple_layout("Hgj", koru_draw::theme::role::row());
    koru_draw::scene::centre_y_in(&reference, rect)
}
