// SPDX-License-Identifier: MPL-2.0

//! The attribute chip: the compact marker a set attribute puts after a row's
//! text — R0's one shared display element (roadmap A2).
//!
//! A chip is not a pill. A pill is *content*: anchored to a character, part of
//! the text, selectable and replaceable. A chip is *presentation of node
//! state*: it trails the text, is never in the layout, and disappears the
//! moment the attribute is cleared. What this module holds is the shaping —
//! the cached label layout, the metrics and the tones. **Painting one is
//! `koru_ui::label_chip`'s** (KoruLabelChip), the one chip component every
//! surface draws through; it reads this cache. Deciding *which* chips a
//! node earns means reading the document, which is `koru-editor`'s
//! (`chips.rs` there), the same split `pill.rs` documents.
//!
//! Two kinds ship in R0: a **tag chip** (the tag's name, tinted by the tag's
//! accent tone) and a **color swatch** (a dot of the node's `koru.color`).
//! A3's date chip and A4's priority chip add [`ChipKind`] variants and reuse
//! this cache and these metrics — that is the extension slot, deliberately.

use std::collections::HashMap;

use parley::Layout;
use peniko::Color;

use crate::theme::{self, Brush};

/// Which recipe a chip draws with.
///
/// `Tag` is a labelled lozenge; `Swatch` is a bare dot. `Due` and `Priority`
/// (A3/A4) are labelled lozenges too. A due date is in the quiet neutral
/// colours (or its list's); a priority wears its level's accent — the
/// mapping is `koru_editor::chips::priority_accent`, chosen by the owner on
/// 2026-11-05 over the old neutral rule. Derived states like overdue are
/// computed by views, never painted here.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum ChipKind {
    Tag,
    Swatch,
    /// The `koru.due` chip: compact date text, neutral tone.
    Due,
    /// The `koru.priority` chip: the level's name, neutral tone.
    Priority,
}

/// Chip metrics, shared by measuring, drawing and hit-testing so the three
/// cannot disagree.
pub const CHIP_H: f32 = 17.0;
pub const CHIP_PAD_X: f32 = 6.0;
/// Air between the text's end and the first chip, and between chips.
pub const CHIP_GAP: f32 = 8.0;
/// The color swatch's diameter.
pub const SWATCH_D: f32 = 10.0;
/// Air between a chip's leading glyph and its label.
///
/// The lane itself is the glyph's own advance — an icon is a glyph and
/// measures like one — so only the gap is a constant here. A chip with no
/// lead reserves neither: unlike the remove lane, which is always there so a
/// hover cannot move a chip, a leading glyph is a property of the *value* and
/// two chips that differ in it are two different drawables anyway.
pub const CHIP_ICON_GAP: f32 = 4.0;
/// The lane a **removable** chip reserves after its label (or, for a swatch,
/// after its dot) for the remove `×` — `koru_ui::label_chip::Trail::Remove`.
///
/// Reserved whether or not the glyph is inked, so a hover cannot make a row's
/// chips shuffle sideways. A chip that cannot be removed reserves none. Wide enough that the whole lane is a comfortable
/// target at the chip's 17px height.
pub const CHIP_CLOSE_W: f32 = 15.0;

/// Everything needed to paint one chip, shaped once and retained.
pub struct ChipVisual {
    pub kind: ChipKind,
    pub width: f32,
    /// `None` for a swatch, which has no text.
    pub label: Option<Layout<Brush>>,
    /// A glyph before the label — the queue a due falls in. Shaped once here
    /// beside the label, so a frame that has drawn this chip lays out nothing.
    pub lead: Option<Layout<Brush>>,
    /// Whether the label is struck through.
    ///
    /// **A field, not `lead == Some(CHECK_SQUARE)`.** The two are equivalent
    /// only because a completed chip is given both, and a paint that inferred
    /// one state from another's glyph is a rule nobody could find later.
    ///
    /// It is deliberately **not** in [`ChipKey`]: it is a pure function of the
    /// same lifecycle that chose the lead, so no two entries can share a key
    /// and disagree about it. Anything that would let a strike vary
    /// independently of the lead has to put it in the key instead.
    pub struck: bool,
}

/// Key: the kind, the label, the accent token (which decides the baked label
/// brush) and the leading glyph. The theme is not in the key — a theme flip
/// clears the cache, the same contract the pill cache has.
///
/// **The glyph is in the key because it is in the width.** `chip_boxes`
/// hit-tests the rectangle it measured, so a chip that painted a lane it had
/// not measured would be pressable where it is not drawn. The accent is here
/// for a different reason and changes no width: it bakes the label's ink.
pub(crate) type ChipKey = (ChipKind, String, Option<String>, Option<char>);

/// Shaped chips, keyed by everything that affects shaping or ink.
#[derive(Default)]
pub struct ChipCache(pub(crate) HashMap<ChipKey, ChipVisual>);

impl ChipCache {
    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn get(
        &self,
        kind: ChipKind,
        label: &str,
        accent: Option<&str>,
        lead: Option<char>,
    ) -> Option<&ChipVisual> {
        self.0.get(&Self::key(kind, label, accent, lead))
    }

    pub(crate) fn key(
        kind: ChipKind,
        label: &str,
        accent: Option<&str>,
        lead: Option<char>,
    ) -> ChipKey {
        (kind, label.to_owned(), accent.map(str::to_owned), lead)
    }
}

/// The fill/ink pair a chip draws with: its accent tone, or the quiet default
/// chip colors for a tag with no color of its own (or a token this build does
/// not know — a peer's newer palette must still render legibly).
pub fn chip_colors(accent: Option<&str>) -> (Color, Color) {
    match accent.and_then(theme::accent_tone) {
        Some(tone) => (tone.fill, tone.ink),
        None => (theme::p().chip_bg, theme::p().text_dim),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::TextEngine;

    /// **A leading glyph is a lane in the width, and the width is what is
    /// hit-tested.** `chip_boxes` measures a chip and then presses the
    /// rectangle it measured, so a glyph the paint knew about and the measure
    /// did not would make the chip pressable where it is not drawn.
    #[test]
    fn a_leading_glyph_widens_the_chip_by_its_own_lane() {
        let mut e = TextEngine::new();
        let plain = e.measure_attr_chip(ChipKind::Due, "Wed", None, None, false);
        let led = e.measure_attr_chip(ChipKind::Due, "Wed", None, Some(crate::icons::CLOCK), false);
        assert!(led > plain, "the lane is in the width: {plain} -> {led}");

        // And it is exactly the glyph's advance plus the gap — no rounding,
        // no second constant.
        let visual = e
            .attr_chips
            .get(ChipKind::Due, "Wed", None, Some(crate::icons::CLOCK))
            .expect("the led chip was cached");
        let lane = visual.lead.as_ref().expect("it has a lead").width() + CHIP_ICON_GAP;
        assert!(
            (led - plain - lane).abs() < 0.01,
            "{led} - {plain} should be the lane {lane}"
        );
    }

    /// Two chips reading the same word with different glyphs are two
    /// drawables, so they are two cache entries. This is what makes the key's
    /// fourth element load-bearing rather than decorative.
    #[test]
    fn the_cache_holds_a_led_chip_and_an_unled_one_apart() {
        let mut e = TextEngine::new();
        e.measure_attr_chip(ChipKind::Due, "Wed", None, None, false);
        e.measure_attr_chip(ChipKind::Due, "Wed", None, Some(crate::icons::CLOCK), false);
        assert!(
            e.attr_chips.get(ChipKind::Due, "Wed", None, None).is_some(),
            "the unled one is still there"
        );
        assert!(
            e.attr_chips
                .get(ChipKind::Due, "Wed", None, Some(crate::icons::CLOCK))
                .is_some(),
            "and the led one is its own entry"
        );
    }

    /// **The strike is paint, not width.** A completed chip is exactly as wide
    /// as an open one with the same label and lead: the line is drawn across
    /// an advance that is already there. If it ever changes the width it has
    /// to enter [`ChipKey`], which is the invariant `struck`'s doc names.
    #[test]
    fn a_struck_chip_measures_the_same_as_an_unstruck_one() {
        let mut e = TextEngine::new();
        let lead = Some(crate::icons::CHECK_SQUARE);
        let open = e.measure_attr_chip(ChipKind::Due, "Wed", None, lead, false);
        // Same key by construction — `struck` is deliberately not in it — so
        // this reads the cached entry back rather than shaping a second one.
        let done = e.measure_attr_chip(ChipKind::Due, "Wed", None, lead, true);
        assert!((open - done).abs() < f32::EPSILON, "{open} vs {done}");
    }

    /// **The swatch takes neither.** It has no label to strike and no text
    /// lane to put a glyph in, so a lead passed for one changes nothing.
    #[test]
    fn a_swatch_ignores_a_lead_and_a_strike() {
        let mut e = TextEngine::new();
        let plain = e.measure_attr_chip(ChipKind::Swatch, "", Some("red"), None, false);
        let led = e.measure_attr_chip(
            ChipKind::Swatch,
            "",
            Some("red"),
            Some(crate::icons::CLOCK),
            true,
        );
        assert!((plain - led).abs() < f32::EPSILON, "a dot is a dot");
        assert!((plain - SWATCH_D).abs() < f32::EPSILON);
    }
}
