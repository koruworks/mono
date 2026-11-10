// SPDX-License-Identifier: MPL-2.0

//! Leading emoji in a document title, used as the page icon in Explore and tabs.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

/// Colour emoji fill their em box more than outline icons, so the page emoji
/// keeps a smaller nominal size than a text tier.
pub const PAGE_EMOJI_SIZE: f32 = 10.0;

/// Slot width reserved for a page emoji (or kind glyph) before the label.
pub const PAGE_ICON_SLOT: f32 = 18.0;

// One rule, one place — draw crate owns the split so pills can ask too.
pub use koru_draw::split_leading_emoji;

/// Draw a page emoji centred in an [`PAGE_ICON_SLOT`]-wide slot at `icon_x`.
///
/// When `label_centre_y` is set, centre the emoji's visible ink on the label's
/// text centre. Emoji font metrics put the baseline below the apparent centre,
/// so aligning baselines makes the icon look low beside a title. With no label,
/// centre the ink in `row`.
pub fn draw_page_emoji(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    icon: &str,
    icon_x: f32,
    row: Rect,
    label_centre_y: Option<f32>,
) {
    let emoji_style = theme::TextStyle::new(
        theme::TextMetrics::new(PAGE_EMOJI_SIZE, PAGE_EMOJI_SIZE * 1.25),
        theme::FontWeight::Medium,
    );
    let glyph = engine.simple_layout(icon, emoji_style);
    let centre_y = label_centre_y.unwrap_or(row.y + row.h * 0.5);
    let glyph_y = scene::centre_y_in(&glyph, Rect::new(icon_x, centre_y, PAGE_ICON_SLOT, 0.0));
    scene::draw_layout(
        scene,
        root,
        &glyph,
        icon_x + (PAGE_ICON_SLOT - glyph.width()) * 0.5,
        glyph_y,
        None,
    );
}
