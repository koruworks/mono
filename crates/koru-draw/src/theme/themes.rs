// SPDX-License-Identifier: MPL-2.0

//! **What a theme is** — ELEMENTS.final §3.4, decision D3.
//!
//! A theme is a named, complete colour table with a stable key and a base
//! appearance, and nothing else: no metric, radius, typography or spacing.
//! It holds a standard and a high-contrast [`Palette`] (contrast is the
//! independent accessibility axis), and names every other hand-tuned set a
//! stable name resolves through — the terminal's, the canvas presets, the
//! caret colours and their selection washes, and the code highlighter's.
//!
//! The process still has one active theme, set by the host, because fourteen
//! callers read it through [`super::p`] and [`super::is_dark`]; those keep
//! working, with the appearance now derived from the theme rather than being
//! its identity. An element reads its theme from the frame context instead,
//! so a subtree can carry another.
//!
//! **Built-in high contrast is a placeholder** (ELEMENTS-PLAN S5): `light` and
//! `dark` name their standard table as their high-contrast one, explicitly,
//! until the designed palettes land with the app-level setting. Nothing calls
//! that high-contrast support.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use peniko::Color;

#[cfg(any(test, feature = "fixture-theme"))]
use super::LIGHT_TABLE;
use super::{
    CANVAS_PRESETS_DARK, CANVAS_PRESETS_LIGHT, DARK, DARK_TERMINAL, LIGHT, LIGHT_TERMINAL, Palette,
    TerminalPalette, rgb,
};

/// What the OS titlebar, the terminal, the code highlighter and "System"
/// read: whether the theme is a light one or a dark one.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

impl Appearance {
    pub fn key(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// Which of a theme's two tables applies.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum Contrast {
    #[default]
    Standard,
    High,
}

impl Contrast {
    pub const ALL: [Self; 2] = [Self::Standard, Self::High];

    pub fn key(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::High => "high",
        }
    }
}

/// The caret colours a person can pick, and the selection wash each one
/// casts, in [`super::CaretColor::ALL`]'s order.
#[derive(Debug, PartialEq)]
pub struct CaretSet {
    pub colors: [Color; 4],
    pub selections: [Color; 4],
}

pub(super) static LIGHT_CARETS: CaretSet = CaretSet {
    colors: [
        rgb(0x19, 0x19, 0x19),
        rgb(0x25, 0x63, 0xEB),
        rgb(0xD9, 0x77, 0x06),
        rgb(0x7C, 0x3A, 0xED),
    ],
    // The caret's hue at about a quarter opacity over the page; the neutral
    // caret casts a neutral wash, not a leftover blue.
    selections: [
        Color::from_rgba8(0x19, 0x19, 0x19, 0x40),
        Color::from_rgba8(0x25, 0x63, 0xEB, 0x40),
        Color::from_rgba8(0xD9, 0x77, 0x06, 0x40),
        Color::from_rgba8(0x7C, 0x3A, 0xED, 0x40),
    ],
};

pub(super) static DARK_CARETS: CaretSet = CaretSet {
    // Violet is a mid violet that still reads on #18181B without blowing out
    // the selection's contrast.
    colors: [
        rgb(0xF4, 0xF4, 0xF5),
        rgb(0x60, 0xA5, 0xFA),
        rgb(0xFB, 0xBF, 0x24),
        rgb(0x8B, 0x5C, 0xF6),
    ],
    // Solid tints at roughly a third of the accent, stronger than light so a
    // highlight still reads on a dark ground while the text stays #F4F4F5.
    selections: [
        rgb(0x3F, 0x3F, 0x46),
        rgb(0x1E, 0x3A, 0x5F),
        rgb(0x5C, 0x3D, 0x12),
        rgb(0x4C, 0x32, 0x78),
    ],
};

/// One theme: its stable key, its appearance, and every table it supplies.
///
/// One struct literal per theme, so the compiler rejects a missing set; a
/// theme that takes another's set names it here, where a reader can see the
/// reuse. There is no runtime fallback between themes.
pub struct Theme {
    /// Stable: siblings may be added, a key is never renamed or removed.
    pub key: &'static str,
    pub appearance: Appearance,
    pub standard: &'static Palette,
    pub high_contrast: &'static Palette,
    pub terminal: &'static TerminalPalette,
    pub canvas_presets: &'static [[u8; 4]; 3],
    pub carets: &'static CaretSet,
    /// The code highlighter's set, named by appearance: the sets live in
    /// `koru-blocks`, which this crate cannot see.
    pub syntax: Appearance,
}

impl std::fmt::Debug for Theme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Theme")
            .field("key", &self.key)
            .field("appearance", &self.appearance)
            .finish_non_exhaustive()
    }
}

impl Theme {
    /// This theme's table at `contrast`.
    pub fn palette(&self, contrast: Contrast) -> &'static Palette {
        match contrast {
            Contrast::Standard => self.standard,
            Contrast::High => self.high_contrast,
        }
    }

    pub fn is_dark(&self) -> bool {
        self.appearance == Appearance::Dark
    }
}

pub static LIGHT_THEME: Theme = Theme {
    key: "light",
    appearance: Appearance::Light,
    standard: &LIGHT,
    // Placeholder (S5): the designed high-contrast table is outstanding.
    high_contrast: &LIGHT,
    terminal: &LIGHT_TERMINAL,
    canvas_presets: &CANVAS_PRESETS_LIGHT,
    carets: &LIGHT_CARETS,
    syntax: Appearance::Light,
};

pub static DARK_THEME: Theme = Theme {
    key: "dark",
    appearance: Appearance::Dark,
    standard: &DARK,
    // Placeholder (S5): the designed high-contrast table is outstanding.
    high_contrast: &DARK,
    terminal: &DARK_TERMINAL,
    canvas_presets: &CANVAS_PRESETS_DARK,
    carets: &DARK_CARETS,
    syntax: Appearance::Dark,
};

/// **The third theme that proves there can be more than two** (D3). Test and
/// preview only — never listed in Settings, never written to preferences,
/// absent from ordinary and release builds. Its colours are deliberately
/// off: every role is the light table's pushed warm, and its high-contrast
/// table the same pushed cool, so a surface that ignores the theme it was
/// given is visible at a glance and the contrast axis is exercised for real.
/// It decides nothing about any production palette. Its other sets are the
/// light theme's, named.
#[cfg(any(test, feature = "fixture-theme"))]
pub static FIXTURE_THEME: Theme = Theme {
    key: "fixture",
    appearance: Appearance::Light,
    standard: &FIXTURE_STANDARD_TABLE,
    high_contrast: &FIXTURE_HIGH_TABLE,
    terminal: &LIGHT_TERMINAL,
    canvas_presets: &CANVAS_PRESETS_LIGHT,
    carets: &LIGHT_CARETS,
    syntax: Appearance::Light,
};

#[cfg(any(test, feature = "fixture-theme"))]
const fn sepia(c: Color) -> Color {
    let [r, g, b, a] = c.components;
    Color::new([r, g * 0.9, b * 0.72, a])
}

#[cfg(any(test, feature = "fixture-theme"))]
const fn slate(c: Color) -> Color {
    let [r, g, b, a] = c.components;
    Color::new([r * 0.72, g * 0.88, b, a])
}

#[cfg(any(test, feature = "fixture-theme"))]
static FIXTURE_STANDARD_TABLE: Palette = FIXTURE_STANDARD;

#[cfg(any(test, feature = "fixture-theme"))]
static FIXTURE_HIGH_TABLE: Palette = FIXTURE_HIGH;

/// Every colour the light table has, pushed warm.
#[cfg(any(test, feature = "fixture-theme"))]
const FIXTURE_STANDARD: Palette = Palette {
    bg: sepia(LIGHT_TABLE.bg),
    canvas: sepia(LIGHT_TABLE.canvas),
    text: sepia(LIGHT_TABLE.text),
    text_dim: sepia(LIGHT_TABLE.text_dim),
    placeholder: sepia(LIGHT_TABLE.placeholder),
    selection: sepia(LIGHT_TABLE.selection),
    caret: sepia(LIGHT_TABLE.caret),
    checkbox: sepia(LIGHT_TABLE.checkbox),
    checkbox_done: sepia(LIGHT_TABLE.checkbox_done),
    checkbox_mark: sepia(LIGHT_TABLE.checkbox_mark),
    panel_bg: sepia(LIGHT_TABLE.panel_bg),
    panel_border: sepia(LIGHT_TABLE.panel_border),
    control_border: sepia(LIGHT_TABLE.control_border),
    panel_shadow: sepia(LIGHT_TABLE.panel_shadow),
    row_selected: sepia(LIGHT_TABLE.row_selected),
    row_hint: sepia(LIGHT_TABLE.row_hint),
    focus_ring: sepia(LIGHT_TABLE.focus_ring),
    chart: LIGHT_TABLE.chart,
    status_bg: sepia(LIGHT_TABLE.status_bg),
    chip_bg: sepia(LIGHT_TABLE.chip_bg),
    ground: sepia(LIGHT_TABLE.ground),
    code_bg: sepia(LIGHT_TABLE.code_bg),
    hover_tint: sepia(LIGHT_TABLE.hover_tint),
    bubble_bg: sepia(LIGHT_TABLE.bubble_bg),
    bubble_fg: sepia(LIGHT_TABLE.bubble_fg),
    bubble_dim: sepia(LIGHT_TABLE.bubble_dim),
    bubble_hover: sepia(LIGHT_TABLE.bubble_hover),
    grip: sepia(LIGHT_TABLE.grip),
    grip_active: sepia(LIGHT_TABLE.grip_active),
    drop_indicator: sepia(LIGHT_TABLE.drop_indicator),
    scrollbar_thumb: sepia(LIGHT_TABLE.scrollbar_thumb),
    highlights: LIGHT_TABLE.highlights,
    workspace_colors: LIGHT_TABLE.workspace_colors,
    close_hover: sepia(LIGHT_TABLE.close_hover),
    close_hover_fg: sepia(LIGHT_TABLE.close_hover_fg),
    notice_bg: sepia(LIGHT_TABLE.notice_bg),
    notice_fg: sepia(LIGHT_TABLE.notice_fg),
    spell: sepia(LIGHT_TABLE.spell),
    accent: sepia(LIGHT_TABLE.accent),
    on_accent: sepia(LIGHT_TABLE.on_accent),
    scrim: sepia(LIGHT_TABLE.scrim),
    document: LIGHT_TABLE.document,
    contact: LIGHT_TABLE.contact,
    date: LIGHT_TABLE.date,
    link: LIGHT_TABLE.link,
    run: LIGHT_TABLE.run,
    accents: LIGHT_TABLE.accents,
};

#[cfg(any(test, feature = "fixture-theme"))]
const FIXTURE_HIGH: Palette = Palette {
    bg: slate(LIGHT_TABLE.bg),
    canvas: slate(LIGHT_TABLE.canvas),
    text: slate(LIGHT_TABLE.text),
    text_dim: slate(LIGHT_TABLE.text_dim),
    placeholder: slate(LIGHT_TABLE.placeholder),
    selection: slate(LIGHT_TABLE.selection),
    caret: slate(LIGHT_TABLE.caret),
    checkbox: slate(LIGHT_TABLE.checkbox),
    checkbox_done: slate(LIGHT_TABLE.checkbox_done),
    checkbox_mark: slate(LIGHT_TABLE.checkbox_mark),
    panel_bg: slate(LIGHT_TABLE.panel_bg),
    panel_border: slate(LIGHT_TABLE.panel_border),
    control_border: slate(LIGHT_TABLE.control_border),
    panel_shadow: slate(LIGHT_TABLE.panel_shadow),
    row_selected: slate(LIGHT_TABLE.row_selected),
    row_hint: slate(LIGHT_TABLE.row_hint),
    focus_ring: slate(LIGHT_TABLE.focus_ring),
    chart: LIGHT_TABLE.chart,
    status_bg: slate(LIGHT_TABLE.status_bg),
    chip_bg: slate(LIGHT_TABLE.chip_bg),
    ground: slate(LIGHT_TABLE.ground),
    code_bg: slate(LIGHT_TABLE.code_bg),
    hover_tint: slate(LIGHT_TABLE.hover_tint),
    bubble_bg: slate(LIGHT_TABLE.bubble_bg),
    bubble_fg: slate(LIGHT_TABLE.bubble_fg),
    bubble_dim: slate(LIGHT_TABLE.bubble_dim),
    bubble_hover: slate(LIGHT_TABLE.bubble_hover),
    grip: slate(LIGHT_TABLE.grip),
    grip_active: slate(LIGHT_TABLE.grip_active),
    drop_indicator: slate(LIGHT_TABLE.drop_indicator),
    scrollbar_thumb: slate(LIGHT_TABLE.scrollbar_thumb),
    highlights: LIGHT_TABLE.highlights,
    workspace_colors: LIGHT_TABLE.workspace_colors,
    close_hover: slate(LIGHT_TABLE.close_hover),
    close_hover_fg: slate(LIGHT_TABLE.close_hover_fg),
    notice_bg: slate(LIGHT_TABLE.notice_bg),
    notice_fg: slate(LIGHT_TABLE.notice_fg),
    spell: slate(LIGHT_TABLE.spell),
    accent: slate(LIGHT_TABLE.accent),
    on_accent: slate(LIGHT_TABLE.on_accent),
    scrim: slate(LIGHT_TABLE.scrim),
    document: LIGHT_TABLE.document,
    contact: LIGHT_TABLE.contact,
    date: LIGHT_TABLE.date,
    link: LIGHT_TABLE.link,
    run: LIGHT_TABLE.run,
    accents: LIGHT_TABLE.accents,
};

/// Every theme this build knows, built-ins first. The fixture is one of them
/// wherever it is compiled, so "every registered theme" includes it.
pub fn registered() -> &'static [&'static Theme] {
    #[cfg(any(test, feature = "fixture-theme"))]
    {
        static ALL: [&Theme; 3] = [&LIGHT_THEME, &DARK_THEME, &FIXTURE_THEME];
        &ALL
    }
    #[cfg(not(any(test, feature = "fixture-theme")))]
    {
        static ALL: [&Theme; 2] = [&LIGHT_THEME, &DARK_THEME];
        &ALL
    }
}

/// The theme registered under `key`.
pub fn by_key(key: &str) -> Option<&'static Theme> {
    registered().iter().copied().find(|theme| theme.key == key)
}

/// The host's root theme, as an index into [`registered`].
static ACTIVE: AtomicU8 = AtomicU8::new(0);
static HIGH_CONTRAST: AtomicBool = AtomicBool::new(false);
/// Set by the fixture preview: the theme it chose holds for the whole run,
/// whatever the preference or the OS says afterwards.
static PINNED: AtomicBool = AtomicBool::new(false);

/// The host's root theme. An element reads its frame context, not this.
pub fn active() -> &'static Theme {
    let all = registered();
    all.get(usize::from(ACTIVE.load(Ordering::Relaxed)))
        .copied()
        .unwrap_or(all[0])
}

/// The host's root contrast.
pub fn contrast() -> Contrast {
    if HIGH_CONTRAST.load(Ordering::Relaxed) {
        Contrast::High
    } else {
        Contrast::Standard
    }
}

/// Make `theme` the root theme. Ignored while a preview has pinned one.
pub fn set_active(theme: &'static Theme) {
    if PINNED.load(Ordering::Relaxed) {
        return;
    }
    store(theme);
}

/// Make `contrast` the root contrast. Ignored while a preview has pinned one.
pub fn set_contrast(contrast: Contrast) {
    if PINNED.load(Ordering::Relaxed) {
        return;
    }
    HIGH_CONTRAST.store(contrast == Contrast::High, Ordering::Relaxed);
}

/// **For the fixture preview only**: apply `theme` at `contrast` for the rest
/// of the process, in place of the preference and the OS, without writing
/// either anywhere.
pub fn pin(theme: &'static Theme, contrast: Contrast) {
    store(theme);
    HIGH_CONTRAST.store(contrast == Contrast::High, Ordering::Relaxed);
    PINNED.store(true, Ordering::Relaxed);
}

fn store(theme: &'static Theme) {
    if let Some(index) = registered()
        .iter()
        .position(|candidate| std::ptr::eq(*candidate, theme))
    {
        ACTIVE.store(index as u8, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
