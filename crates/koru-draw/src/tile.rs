// SPDX-License-Identifier: MPL-2.0

//! The rounded tile a workspace wears: its colour, and its emoji or its initial.
//!
//! **One component, drawn at five sizes.** It appears in the pane's header, in
//! every switcher row, in every row of the manager's table, in that table's
//! details panel, and as the preview in the icon picker — and it had been
//! written out five times, each with its own idea of how big the glyph should
//! be relative to the tile and where the middle was. Two of those five put the
//! emoji visibly high.
//!
//! A caller now changes one number. Everything else — the corner radius, the
//! glyph size, whether the fallback is bold — is derived from it, so the tile
//! looks like itself at 18 px and at 48 px.
//!
//! It lives in `koru-draw` because both the pane chrome (the switcher, the
//! header) and `apps/koru` (the table, the picker) draw one, and those two
//! cannot see each other.

use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;

use crate::geometry::Rect;
use crate::text::TextEngine;
use crate::{scene, theme};

/// How much of the tile the glyph fills.
///
/// **Emoji are the constraint.** A colour emoji fills its em box far more
/// completely than a Latin letter does, so a glyph size that flatters a
/// monogram makes an emoji look crammed. This is tuned for the emoji; the
/// monogram gets a little more (below) to compensate for the other direction.
const GLYPH_OF_TILE: f32 = 0.46;
/// A letter has more air around it than an emoji at the same nominal size, so
/// it is set slightly larger to look the same weight.
const MONOGRAM_OF_TILE: f32 = 0.52;
/// Corner radius as a fraction of the size, so a small tile is not a circle and
/// a large one is not a square.
const RADIUS_OF_TILE: f32 = 0.22;

fn appearance(color: Option<&str>, p: &theme::Palette) -> (Color, bool) {
    match color {
        None => (p.panel_bg, true),
        Some(name) => (
            p.workspace_colors
                .iter()
                .find(|(n, _)| *n == name)
                .map_or(p.chip_bg, |(_, c)| *c),
            false,
        ),
    }
}

/// What to draw. Borrowed rather than owned: every caller already has these.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Tile<'a> {
    /// The emoji, when one is set.
    pub icon: Option<&'a str>,
    /// A name from [`theme::WORKSPACE_COLORS`]. Absent uses the adaptive panel
    /// surface; an unknown stored name uses the neutral chip color so it is
    /// never mistaken for another named identity.
    pub color: Option<&'a str>,
    /// Where the initial comes from, when there is no emoji.
    pub name: &'a str,
}

impl Tile<'_> {
    /// Draw it filling `at`. Square is assumed; pass a square rect.
    pub fn draw(&self, scene: &mut Scene, root: Affine, engine: &mut TextEngine, at: Rect) {
        self.draw_in(scene, root, engine, at, theme::p());
    }

    /// [`draw`](Self::draw) in `p`, the table of the theme it is shown
    /// under: an element subtree with its own theme paints its tiles in it.
    pub fn draw_in(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        p: &theme::Palette,
    ) {
        let size = at.w.min(at.h);
        let (tint, bordered) = appearance(self.color, p);
        scene::fill(
            scene,
            root,
            tint,
            &scene::rrect(at.x, at.y, at.w, at.h, size * RADIUS_OF_TILE),
        );
        if bordered {
            scene::stroke(
                scene,
                root,
                p.panel_border,
                theme::BORDER_W,
                &scene::rrect(at.x, at.y, at.w, at.h, size * RADIUS_OF_TILE),
            );
        }

        let (text, colour, weight, scale) = match self.icon {
            // `None` for the colour: a colour emoji draws its own, and
            // overriding it would flatten it to a silhouette.
            Some(icon) => (
                icon.to_string(),
                None,
                theme::FontWeight::Medium,
                GLYPH_OF_TILE,
            ),
            None => (
                self.name
                    .chars()
                    .next()
                    .unwrap_or('W')
                    .to_uppercase()
                    .to_string(),
                // The panel's own foreground rather than a guess at contrast
                // with a colour the user picked.
                Some(p.text),
                theme::FontWeight::SemiBold,
                MONOGRAM_OF_TILE,
            ),
        };
        // A tile is sized by its own geometry, not by a text tier: the glyph is
        // a fraction of the square it sits in, so the size is computed and the
        // leading is irrelevant — `centre_in` places it off the ink.
        let glyph = size * scale;
        let layout = engine.simple_layout(
            &text,
            theme::TextStyle::new(theme::TextMetrics::new(glyph, glyph), weight),
        );
        // **`centre_in`, not `(h - height) * 0.5`.** Centring the line box
        // centres the leading rather than the glyphs, which is what left every
        // emoji sitting high — see `scene::centre_in`.
        let (x, y) = scene::centre_in(&layout, at);
        scene::draw_layout(scene, root, &layout, x, y, colour);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_is_the_adaptive_panel_surface_but_unknown_remains_neutral() {
        let p = theme::p();
        assert_eq!(appearance(None, p), (p.panel_bg, true));
        assert_eq!(appearance(Some("future-color"), p), (p.chip_bg, false));
    }
}
