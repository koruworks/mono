// SPDX-License-Identifier: MPL-2.0

//! The floating panel every document menu is built on.
//!
//! There are four of them now — the page settings behind the title's `…`, the
//! block-type dropdown and the overflow in the selection bubble, and the
//! list-style submenu — where there was one. Four copies of "shadow, rounded
//! rect, hairline, rows of `ROW_H`, `hover_tint` on the row under the pointer"
//! is four chances for one of them to drift, so the recipe lives here and each
//! menu supplies only its rows.
//!
//! **One function owns the geometry.** [`Menu::row_rect`] is called by the
//! renderer and by [`Menu::item_at`] alike, so a row is clickable exactly where
//! it is drawn — the rule in `docs/features/design/DESIGN.md`. The rows are passed in on every
//! call rather than owned, because they are a pure function of the document and
//! recomputing them is cheaper than keeping them in step.
//!
//! It knows nothing about the editor: what a row *means*, and which one is
//! current, is decided by `BuiltInDocument` and passed in at draw time — the
//! same split the rest of the chrome uses, and what keeps the geometry testable
//! without a viewport or a `WorkspaceDoc`.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;

/// What sits at the right-hand end of a row.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub enum Trailing {
    #[default]
    None,
    /// This row is the current choice.
    Check,
    /// An on/off switch. Drawn dead when the row is disabled, which is how
    /// "coming soon" is said without a second row type.
    Toggle(bool),
    /// A quiet note in `text_dim` — "Coming soon", a chord.
    Note(&'static str),
    /// A row of color swatches, the one of which is selected. Hit-tested by
    /// [`Menu::swatch_at`], so a swatch is picked rather than cycled.
    Swatches(usize),
}

/// What a menu row prints in its shortcut column.
///
/// **The registry's type, under this module's old name.** It began here — two
/// shapes, because two kinds of thing live in that column: a binding this app
/// owns, looked up so the chord a menu teaches and the chord that fires are one
/// value (they used to be 77 separate `"Ctrl+…"` literals, every one of them
/// wrong on macOS), and a chord the *platform* owns, which is not ours to
/// rebind. It moved down to `koru-keys` when the tooltips turned out to need
/// exactly the same thing, and a second copy of it would have been the same
/// mistake one layer up. Every row here keeps reading `Chord::Id(…)`.
pub use koru_keys::Hinted as Chord;

/// One row of a menu.
///
/// `Clone` rather than `Copy` since spelling suggestions arrived: a label can
/// now be a runtime `String` (a suggested word), carried as a `Cow` so the
/// overwhelmingly common static label still costs nothing to build.
#[derive(Clone, PartialEq, Debug)]
pub enum Row {
    /// A small caps heading. Not selectable, and the keyboard steps over it.
    Heading(&'static str),
    /// A heading with a settings icon at its right end — the sliders beside
    /// *PAGE SETTINGS*, which open the global settings. Unlike a plain
    /// heading it can be chosen: the arrow keys stop on it and Enter follows
    /// the link, and a click anywhere on the heading's band does too, because
    /// the band is the row's hit area like every other row's. `link` is the
    /// icon's name in words, *Global settings*, for whoever cannot see it.
    HeadingLink {
        heading: &'static str,
        link: &'static str,
    },
    /// A hairline between groups. Not selectable.
    Divider,
    Item {
        /// A Phosphor glyph, or `None` for a row whose leading column is drawn
        /// by the caller's `leading` callback (a page-width diagram, a `T`).
        icon: Option<char>,
        label: std::borrow::Cow<'static, str>,
        /// The chord that does the same thing, set quietly to the left of the
        /// trailing column.
        ///
        /// Its own field rather than a [`Trailing`] variant because a row needs
        /// both: *Align centre* has a chord **and** a tick when the caret is in
        /// a centred line, and a menu that dropped one to show the other would
        /// stop teaching its shortcuts exactly when you were using them.
        shortcut: Option<Chord>,
        trailing: Trailing,
        enabled: bool,
        /// Named in red while it can be chosen, and dimmed like every other
        /// disabled row when it cannot — the idiom `koru_ui::menu` already
        /// follows, copied rather than invented, because a red row you cannot
        /// press reads as an error rather than as a warning.
        destructive: bool,
    },
}

impl Row {
    /// A plain enabled row with nothing at either end.
    pub const fn item(label: &'static str) -> Self {
        Self::Item {
            icon: None,
            label: std::borrow::Cow::Borrowed(label),
            shortcut: None,
            trailing: Trailing::None,
            enabled: true,
            destructive: false,
        }
    }

    /// A row whose label exists only at runtime — a spelling suggestion.
    pub fn text(label: String) -> Self {
        Self::Item {
            icon: None,
            label: std::borrow::Cow::Owned(label),
            shortcut: None,
            trailing: Trailing::None,
            enabled: true,
            destructive: false,
        }
    }

    // These lost `const` when `label` became a `Cow`: a potentially owned
    // field cannot pass through a `const fn`'s destructor rules, and nothing
    // built a row in const position anyway.
    pub fn with_icon(self, glyph: char) -> Self {
        match self {
            Self::Item {
                label,
                shortcut,
                trailing,
                enabled,
                destructive,
                ..
            } => Self::Item {
                icon: Some(glyph),
                label,
                shortcut,
                trailing,
                enabled,
                destructive,
            },
            other => other,
        }
    }

    /// This row takes something away. See the field.
    pub fn destructive(self) -> Self {
        match self {
            Self::Item {
                icon,
                label,
                shortcut,
                trailing,
                enabled,
                ..
            } => Self::Item {
                icon,
                label,
                shortcut,
                trailing,
                enabled,
                destructive: true,
            },
            other => other,
        }
    }

    /// The chord that does the same thing, if there is one.
    /// Attach a chord only if there is one on this platform.
    ///
    /// `Win+Down` and friends are Windows' own window-management chords and do
    /// not exist elsewhere; a row that advertises one on macOS teaches a key
    /// combination that does nothing.
    pub fn with_optional_shortcut(self, chord: Option<Chord>) -> Self {
        match chord {
            Some(c) => self.with_shortcut(c),
            None => self,
        }
    }

    pub fn with_shortcut(self, chord: Chord) -> Self {
        match self {
            Self::Item {
                icon,
                label,
                trailing,
                enabled,
                destructive,
                ..
            } => Self::Item {
                icon,
                label,
                shortcut: Some(chord),
                trailing,
                enabled,
                destructive,
            },
            other => other,
        }
    }

    pub fn with_trailing(self, trailing: Trailing) -> Self {
        match self {
            Self::Item {
                icon,
                label,
                shortcut,
                enabled,
                destructive,
                ..
            } => Self::Item {
                icon,
                label,
                shortcut,
                trailing,
                enabled,
                destructive,
            },
            other => other,
        }
    }

    pub fn checked(self, on: bool) -> Self {
        if on {
            self.with_trailing(Trailing::Check)
        } else {
            self
        }
    }

    pub fn enabled(self, on: bool) -> Self {
        match self {
            Self::Item {
                icon,
                label,
                shortcut,
                trailing,
                destructive,
                ..
            } => Self::Item {
                icon,
                label,
                shortcut,
                trailing,
                enabled: on,
                destructive,
            },
            other => other,
        }
    }

    /// Whether a click or an arrow key can land here. A heading and a hairline
    /// are furniture: the keyboard steps straight over them, which is what
    /// `explore.rs`'s popover does for the same reason.
    pub fn selectable(&self) -> bool {
        matches!(
            self,
            Self::Item { enabled: true, .. } | Self::HeadingLink { .. }
        )
    }

    /// This row's own height. Not every row is `ROW_H`: a heading is a label
    /// and a hairline is a line, and giving them a full row of air made the
    /// groups read as further apart than they are.
    fn height(&self) -> f32 {
        match self {
            Self::Heading(_) | Self::HeadingLink { .. } => HEADING_H,
            Self::Divider => DIVIDER_H,
            Self::Item { .. } => ITEM_H,
        }
    }
}

// Panel metrics, in step with the editor's overlay panels.
const ITEM_H: f32 = theme::ROW_H;
const HEADING_H: f32 = 22.0;
const DIVIDER_H: f32 = 9.0;
const PAD: f32 = theme::PANEL_PAD;
pub const RADIUS: f32 = 8.0;
const EDGE_MARGIN: f32 = 4.0;
/// Where the leading (icon) column ends and the label begins.
///
/// Wide enough for the icon *and* a gap: at 30 the widest glyph finished four
/// pixels before the text and the two read as one word.
const ICON_W: f32 = 40.0;
/// A color swatch in a [`Trailing::Swatches`] row, and the gap between two.
const SWATCH: f32 = 14.0;
const SWATCH_GAP: f32 = 6.0;
/// The column a trailing tick or toggle occupies, reserved so a shortcut set
/// beside one lands in the same place whether or not the tick is there.
const TRAILING_W: f32 = 20.0;
/// Gap between label and shortcut column (matches koru-ui intent).
const SHORTCUT_GAP: f32 = 28.0;
/// What the chord and the trailing column keep clear of the panel's right
/// edge. One constant, read by the measurement and by the label rule, so the
/// two cannot disagree by a few pixels and elide a label that fits.
const RIGHT_INSET: f32 = 6.0;
/// Design bounds for measured context menus (`DESIGN - Right Menu Context`).
pub const MIN_W: f32 = 180.0;
pub const MAX_W: f32 = 300.0;

/// A floating menu anchored under the control that opened it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Menu {
    /// The opener's rect in window space. Kept rather than a bare point so the
    /// panel can left-align under the button the way a native dropdown does.
    pub anchor: Rect,
    pub width: f32,
    /// The row index under the pointer, or the one the keyboard has walked to.
    pub hover: Option<usize>,
}

/// How wide the panel must be for `rows`: icon column + label + optional
/// shortcut/trailing. Clamped so a two-letter menu is not a chip and a long
/// label does not become a dialog. Labels that still overflow at `MAX_W` are
/// ellipsized at draw time.
pub fn measure_width(engine: &mut TextEngine, rows: &[Row]) -> f32 {
    // **One reservation for the whole menu**, exactly the one `label_max_w`
    // subtracts. It used to be computed per row here and per *menu* there, so
    // a row with no chord was measured without the chord column and then drawn
    // with it taken away — and elided. That stayed hidden while the widest row
    // in every menu happened to be one that had a chord; the due chip's menu
    // stopped being one when *Go to task* took the click hint, and *Edit due
    // date…* immediately read *Edit du…*.
    let right = right_reservation(engine, rows);
    let mut widest: f32 = 0.0;
    for row in rows {
        let Row::Item { label, .. } = row else {
            continue;
        };
        widest =
            widest.max(ICON_W + engine.simple_layout(label, theme::role::row()).width() + right);
    }
    widest.clamp(MIN_W, MAX_W)
}

/// Everything to the right of a row's label: the pad and inset every row has,
/// the trailing column when any row needs one, and the widest chord with its
/// gap when any row has one.
///
/// **Per menu, not per row**, because the columns line up: a chord on one row
/// reserves that column on all of them, which is what keeps the ticks and
/// chords in a line. Shared by the measurement and the draw so the two cannot
/// disagree about where a label ends.
fn right_reservation(engine: &mut TextEngine, rows: &[Row]) -> f32 {
    let mut chord_w: f32 = 0.0;
    let mut any_chord = false;
    let mut any_trailing = false;
    for row in rows {
        if let Row::Item {
            shortcut, trailing, ..
        } = row
        {
            if let Some(text) =
                shortcut.and_then(|c| koru_keys::show_hint(c, koru_keys::Section::Menu))
            {
                any_chord = true;
                chord_w = chord_w.max(engine.simple_layout(&text, theme::role::hint()).width());
            }
            if !matches!(trailing, Trailing::None) {
                any_trailing = true;
            }
        }
    }
    let mut right = PAD + RIGHT_INSET;
    if any_trailing || any_chord {
        right += TRAILING_W;
    }
    if any_chord {
        right += SHORTCUT_GAP + chord_w;
    }
    right
}

impl Menu {
    pub fn new(anchor: Rect, width: f32) -> Self {
        Self {
            anchor,
            width,
            hover: None,
        }
    }

    fn height(rows: &[Row]) -> f32 {
        PAD * 2.0 + rows.iter().map(Row::height).sum::<f32>()
    }

    /// Label column budget inside a panel of this width, given the widest
    /// shortcut among `rows` (so chords stay visible and labels ellipsize).
    fn label_max_w(&self, engine: &mut TextEngine, rows: &[Row]) -> f32 {
        (self.width - ICON_W - right_reservation(engine, rows)).max(0.0)
    }

    /// Where the menu sits in window space: left-aligned under the opener, then
    /// clamped inside the viewport and flipped above it near the floor.
    pub fn panel(&self, rows: &[Row], viewport: (f32, f32)) -> Rect {
        let h = Self::height(rows);
        let mut x = self.anchor.x;
        let mut y = self.anchor.y + self.anchor.h + 4.0;
        if x + self.width > viewport.0 - EDGE_MARGIN {
            x = (viewport.0 - EDGE_MARGIN - self.width).max(EDGE_MARGIN);
        }
        x = x.max(EDGE_MARGIN);
        if y + h > viewport.1 - EDGE_MARGIN {
            y = (self.anchor.y - h - 4.0).max(EDGE_MARGIN);
        }
        Rect::new(x, y, self.width, h)
    }

    /// The band row `i` occupies. Rows have different heights, so this walks
    /// them rather than multiplying — and it is the only place that does.
    pub fn row_rect(&self, rows: &[Row], viewport: (f32, f32), i: usize) -> Rect {
        let p = self.panel(rows, viewport);
        let mut y = p.y + PAD;
        for row in rows.iter().take(i) {
            y += row.height();
        }
        let h = rows.get(i).map_or(ITEM_H, Row::height);
        Rect::new(p.x, y, p.w, h)
    }

    /// Which selectable row sits under `(x, y)`, or `None` outside the menu —
    /// which is what dismisses it.
    pub fn item_at(&self, rows: &[Row], viewport: (f32, f32), x: f32, y: f32) -> Option<usize> {
        if !self.panel(rows, viewport).contains(x, y) {
            return None;
        }
        (0..rows.len())
            .find(|&i| rows[i].selectable() && self.row_rect(rows, viewport, i).contains(x, y))
    }

    /// Which swatch of a [`Trailing::Swatches`] row sits under `x`.
    pub fn swatch_at(
        &self,
        rows: &[Row],
        viewport: (f32, f32),
        i: usize,
        count: usize,
        x: f32,
    ) -> Option<usize> {
        let r = self.row_rect(rows, viewport, i);
        (0..count).find(|&n| swatch_rect(r, count, n).contains(x, r.y + r.h * 0.5))
    }

    /// Walk the hover to the next selectable row, skipping headings, hairlines
    /// and anything disabled. Wraps, and starts at either end.
    pub fn step(&mut self, rows: &[Row], delta: i32) {
        let selectable: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].selectable()).collect();
        if selectable.is_empty() {
            return;
        }
        let here = self
            .hover
            .and_then(|h| selectable.iter().position(|&i| i == h));
        let next = match here {
            Some(pos) => {
                let len = selectable.len() as i32;
                (((pos as i32 + delta) % len + len) % len) as usize
            }
            None if delta < 0 => selectable.len() - 1,
            None => 0,
        };
        self.hover = Some(selectable[next]);
    }

    /// Paint the menu.
    ///
    /// `swatches` supplies the colors for any [`Trailing::Swatches`] row, and
    /// `leading` draws the icon column for a row that has no glyph — a page
    /// diagram, a letter. Both are callbacks rather than data because their
    /// pixels belong to the menu that asked for them, not to this recipe.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rows: &[Row],
        viewport: (f32, f32),
        swatches: &[Color],
        mut leading: impl FnMut(&mut Scene, Affine, usize, Rect, Color),
    ) {
        let p = self.panel(rows, viewport);
        let pal = theme::p();

        scene::floating_panel(
            scene,
            root,
            p.x,
            p.y,
            p.w,
            p.h,
            &scene::Panel::popover(RADIUS),
        );

        for (i, row) in rows.iter().enumerate() {
            let r = self.row_rect(rows, viewport, i);
            match row {
                Row::Heading(text) => {
                    let laid = engine.simple_layout(
                        text,
                        theme::role::hint().with_weight(theme::FontWeight::SemiBold),
                    );
                    let line_h = scene::first_line_height(&laid);
                    scene::draw_layout_first_line(
                        scene,
                        root,
                        &laid,
                        r.x + PAD + 4.0,
                        r.y + (r.h - line_h) * 0.5,
                        Some(pal.text_dim),
                    );
                }
                Row::HeadingLink { heading, link: _ } => {
                    let laid = engine.simple_layout(
                        heading,
                        theme::role::hint().with_weight(theme::FontWeight::SemiBold),
                    );
                    let line_h = scene::first_line_height(&laid);
                    scene::draw_layout_first_line(
                        scene,
                        root,
                        &laid,
                        r.x + PAD + 4.0,
                        r.y + (r.h - line_h) * 0.5,
                        Some(pal.text_dim),
                    );
                    // The settings sliders, quiet like the heading, on an icon
                    // button's tint and in the text color while the row is
                    // under the pointer or the keyboard.
                    let icon = engine.icon_layout(koru_draw::icons::FADERS, theme::CAPTION_ICON);
                    let side = r.h.min(theme::CAPTION_ICON + 8.0);
                    let x = r.x + r.w - PAD - side;
                    let y = r.y + (r.h - side) * 0.5;
                    let on = self.hover == Some(i);
                    if on {
                        scene::fill(
                            scene,
                            root,
                            pal.hover_tint,
                            &scene::rrect(x, y, side, side, 5.0),
                        );
                    }
                    scene::draw_layout(
                        scene,
                        root,
                        &icon,
                        x + (side - icon.width()) * 0.5,
                        y + (side - icon.height()) * 0.5,
                        Some(if on { pal.text } else { pal.text_dim }),
                    );
                }
                Row::Divider => {
                    scene::fill(
                        scene,
                        root,
                        pal.panel_border,
                        &scene::rect(
                            r.x + PAD + 4.0,
                            r.y + r.h * 0.5,
                            r.w - (PAD + 4.0) * 2.0,
                            1.0,
                        ),
                    );
                }
                Row::Item {
                    icon,
                    label,
                    shortcut,
                    trailing,
                    enabled,
                    destructive,
                } => {
                    if *enabled && self.hover == Some(i) {
                        scene::fill(
                            scene,
                            root,
                            pal.hover_tint,
                            &scene::rrect(r.x + 4.0, r.y, r.w - 8.0, r.h, 5.0),
                        );
                    }
                    // `koru_ui::menu`'s three-way answer, to the letter: a
                    // disabled row is dim whatever it would have taken away.
                    let fg = match (*enabled, *destructive) {
                        (false, _) => pal.text_dim,
                        (true, true) => pal.close_hover,
                        (true, false) => pal.text,
                    };
                    match icon {
                        Some(glyph) => {
                            let laid = engine.icon_layout(*glyph, theme::CAPTION_ICON);
                            scene::draw_layout(
                                scene,
                                root,
                                &laid,
                                r.x + PAD + 4.0,
                                r.y + (r.h - laid.height()) * 0.5,
                                Some(fg),
                            );
                        }
                        None => leading(
                            scene,
                            root,
                            i,
                            Rect::new(r.x + PAD + 4.0, r.y, ICON_W - PAD - 4.0, r.h),
                            fg,
                        ),
                    }
                    let label_max = self.label_max_w(engine, rows);
                    let shown = engine.elided(label, theme::role::row(), label_max);
                    let laid = engine.simple_layout(&shown, theme::role::row());
                    let line_h = scene::first_line_height(&laid);
                    scene::draw_layout_first_line(
                        scene,
                        root,
                        &laid,
                        r.x + ICON_W,
                        r.y + (r.h - line_h) * 0.5,
                        Some(fg),
                    );
                    // The chord sits left of whatever the trailing column
                    // holds, so a ticked row shows both — and it is asked for
                    // rather than formatted, because whether a menu teaches its
                    // chords at all is one decision for every row in the app.
                    if let Some(text) =
                        shortcut.and_then(|c| koru_keys::show_hint(c, koru_keys::Section::Menu))
                    {
                        let laid = engine.simple_layout(&text, theme::role::hint());
                        let line_h = scene::first_line_height(&laid);
                        scene::draw_layout_first_line(
                            scene,
                            root,
                            &laid,
                            r.x + r.w - laid.width() - PAD - 6.0 - TRAILING_W,
                            r.y + (r.h - line_h) * 0.5,
                            Some(pal.text_dim),
                        );
                    }
                    self.draw_trailing(scene, root, engine, r, *trailing, *enabled, swatches);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_trailing(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        r: Rect,
        trailing: Trailing,
        enabled: bool,
        swatches: &[Color],
    ) {
        let pal = theme::p();
        match trailing {
            Trailing::None => {}
            Trailing::Check => {
                let laid = engine.icon_layout(koru_draw::icons::CHECK, theme::CAPTION_ICON);
                scene::draw_layout(
                    scene,
                    root,
                    &laid,
                    r.x + r.w - laid.width() - PAD - 6.0,
                    r.y + (r.h - laid.height()) * 0.5,
                    Some(pal.text),
                );
            }
            Trailing::Note(note) => {
                let laid = engine.simple_layout(note, theme::role::hint());
                let line_h = scene::first_line_height(&laid);
                scene::draw_layout_first_line(
                    scene,
                    root,
                    &laid,
                    r.x + r.w - laid.width() - PAD - 6.0,
                    r.y + (r.h - line_h) * 0.5,
                    Some(pal.text_dim),
                );
            }
            Trailing::Toggle(on) => {
                let (tw, th) = (26.0, 15.0);
                let tx = r.x + r.w - tw - PAD - 6.0;
                let ty = r.y + (r.h - th) * 0.5;
                let track = if on && enabled {
                    pal.accent
                } else {
                    pal.chip_bg
                };
                scene::fill(scene, root, track, &scene::rrect(tx, ty, tw, th, th * 0.5));
                let knob = if on { tx + tw - th + 1.5 } else { tx + 1.5 };
                let fg = if enabled {
                    pal.panel_bg
                } else {
                    pal.placeholder
                };
                scene::fill(
                    scene,
                    root,
                    fg,
                    &scene::rrect(knob, ty + 1.5, th - 3.0, th - 3.0, (th - 3.0) * 0.5),
                );
            }
            Trailing::Swatches(selected) => {
                for (n, color) in swatches.iter().enumerate() {
                    let sr = swatch_rect(r, swatches.len(), n);
                    scene::fill(
                        scene,
                        root,
                        *color,
                        &scene::rrect(sr.x, sr.y, sr.w, sr.h, sr.w * 0.5),
                    );
                    // The chosen swatch is named by its *colour*, not by a
                    // heavier ring: one border width, like everything else.
                    scene::stroke(
                        scene,
                        root,
                        if n == selected {
                            pal.text
                        } else {
                            pal.panel_border
                        },
                        theme::BORDER_W,
                        &scene::rrect(sr.x, sr.y, sr.w, sr.h, sr.w * 0.5),
                    );
                }
            }
        }
    }
}

/// Where swatch `n` of `count` sits within a row — the one arithmetic, so the
/// drawn circle and [`Menu::swatch_at`] cannot disagree.
///
/// The spacing comes from `scene::cell_rect`, shared with the workspace
/// picker's palette; what is local is where the strip *starts*, since this one
/// is right-aligned in its row and that one is left-aligned under a heading.
fn swatch_rect(r: Rect, count: usize, n: usize) -> Rect {
    let x0 = r.x + r.w - PAD - 6.0 - scene::cells_width(count, SWATCH, SWATCH_GAP);
    scene::cell_rect(x0, r.y + (r.h - SWATCH) * 0.5, SWATCH, SWATCH_GAP, n)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod width_tests {
    use super::*;

    /// **Every row is measured with the column every row is drawn with.**
    ///
    /// The measurement added the chord column only to rows that had a chord;
    /// the draw subtracted the widest chord from *every* row. A menu whose
    /// widest row had no chord was therefore sized too narrow and then elided
    /// — which is how the due chip's menu came to read *Edit du…* the moment
    /// *Go to task* took the click hint off the row below it.
    #[test]
    fn a_row_without_a_chord_is_measured_with_the_chord_column_the_draw_reserves() {
        let mut engine = TextEngine::new();
        let rows = vec![
            Row::item("Go").with_shortcut(Chord::Literal("Click")),
            Row::item("Edit due date\u{2026}"),
        ];
        let width = measure_width(&mut engine, &rows);
        let menu = Menu::new(Rect::new(0.0, 0.0, 10.0, 10.0), width);

        let budget = menu.label_max_w(&mut engine, &rows);
        let longest = engine
            .simple_layout("Edit due date\u{2026}", theme::role::row())
            .width();

        assert!(
            width < MAX_W,
            "the fixture must not be clamped, or elision is expected"
        );
        // Adding then subtracting the reserved columns can lose one f32 ULP,
        // depending on the fallback font's fractional advances.
        let rounding_tolerance = f32::EPSILON * width;
        assert!(
            budget + rounding_tolerance >= longest,
            "measured {width}, budget {budget}, label {longest}: the label does not fit the width it was measured into"
        );
    }
}
