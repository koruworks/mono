// SPDX-License-Identifier: MPL-2.0

//! **Drawing an agent's or provider's mark**: the picker's rail and a
//! favorite's row wear it through their leaf (`paint::ProviderMark`), and a
//! host draws the same mark with it elsewhere. Kept apart from the leaves,
//! as the chat kept it, because it paints outside an element.

use koru_draw::brand::ProfileMark;
use koru_draw::geometry::Rect;
use koru_draw::scene;
use koru_draw::text::TextEngine;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;

/// **A mark in `inner`**: a brand's vector mark, or a glyph centred in it —
/// what a rail entry and a favorite's row wear, and what a host draws the
/// same mark with elsewhere (the chat's context ring).
pub fn draw_mark(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    mark: ProfileMark,
    inner: Rect,
    color: Color,
) {
    match mark {
        ProfileMark::Brand(icon) => scene::draw_brand_icon(canvas, root, inner, color, icon),
        ProfileMark::Glyph(glyph) => {
            let layout = engine.icon_layout(glyph, inner.h);
            scene::draw_layout(
                canvas,
                root,
                &layout,
                inner.x + (inner.w - layout.width()) * 0.5,
                inner.y + (inner.h - layout.height()) * 0.5,
                Some(color),
            );
        }
    }
}
