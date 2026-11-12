// SPDX-License-Identifier: MPL-2.0

//! `KoruAvatar`: a person's round chip — today, the initial of their name.
//!
//! **The smallest form** (welcome step 1, TECHNICAL.final D19/D21): an
//! initial in a circle, drawn on the surface a Profile with no color is
//! drawn on everywhere after it is created — the panel background with a
//! hairline border, the letter in the text color — so the chip a person sees
//! while typing their name is the one their Profile wears afterwards. A
//! color keyed on the name would change with every keystroke. With no name
//! yet it is an empty circle.
//!
//! The letter rule is [`koru_draw::tile::Tile`]'s: the name's first character,
//! upper-cased. A picture and an emoji arrive in step 2; the chip is
//! decorative (a host names the field beside it, not the chip).
//!
//! One inspector component, `KoruAvatar`, with what it drew.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{inspect, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

/// How much of the circle the letter fills: the tile's monogram share, so a
/// chip and a tile of one size carry the same weight of letter.
const MONOGRAM_OF_SIZE: f32 = 0.52;

/// What to draw: the name the initial comes from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct KoruAvatar<'a> {
    name: &'a str,
}

impl<'a> KoruAvatar<'a> {
    /// The chip for `name`: its initial, or an empty circle while it is
    /// blank.
    pub fn initial(name: &'a str) -> Self {
        Self { name }
    }

    /// The letter drawn: the name's first character, upper-cased; `None`
    /// for a blank name.
    pub fn letter(&self) -> Option<String> {
        self.name
            .trim()
            .chars()
            .next()
            .map(|first| first.to_uppercase().to_string())
    }

    /// Draw the chip filling the square `at`, in `palette` — the table of the
    /// theme it is shown under, so an element subtree with its own theme
    /// paints its chip in it.
    #[track_caller]
    pub fn draw_in(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        palette: &theme::Palette,
    ) {
        let letter = self.letter();
        let _component = inspect::scope(
            "KoruAvatar",
            "component",
            root,
            scene::rect(at.x, at.y, at.w, at.h),
            |record| {
                record.property(
                    "implementation",
                    "koru_ui::avatar::KoruAvatar",
                    "shared implementation",
                );
                record.property(
                    "drew",
                    if letter.is_some() { "initial" } else { "empty" },
                    "caller name",
                );
                record.property(
                    "surface",
                    "panel_bg + panel_border",
                    "a Profile with no color (Tile's no-color surface)",
                );
                record.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        let size = at.w.min(at.h);
        if size <= 0.0 {
            return;
        }
        let (cx, cy) = at.centre();
        let radius = size * 0.5;
        let disc = scene::circle(cx, cy, radius);
        scene::fill(canvas, root, palette.panel_bg, &disc);
        scene::stroke(
            canvas,
            root,
            palette.panel_border,
            theme::BORDER_W,
            &scene::circle(cx, cy, radius - theme::BORDER_W as f32 * 0.5),
        );
        let Some(letter) = letter else {
            return;
        };
        let glyph = size * MONOGRAM_OF_SIZE;
        let layout = engine.simple_layout(
            &letter,
            theme::TextStyle::new(
                theme::TextMetrics::new(glyph, glyph),
                theme::FontWeight::SemiBold,
            ),
        );
        // Centred on the ink, as the tile's letter is.
        let (x, y) = scene::centre_in(&layout, at);
        scene::draw_layout(canvas, root, &layout, x, y, Some(palette.text));
    }
}

#[cfg(test)]
mod tests;
