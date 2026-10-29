// SPDX-License-Identifier: MPL-2.0

use koru_draw::theme::{self, Density, FontChoice, TextMetrics};
use koru_session_protocol::{CellStyle, TerminalColor};
use peniko::Color;
use serde::Serialize;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalFace {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedCellStyle {
    pub foreground: Color,
    pub background: Color,
    pub face: TerminalFace,
    pub underline: bool,
    pub strikeout: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyleEpoch {
    pub dark: bool,
    pub density: Density,
    pub font: FontChoice,
    /// An installed family the person named, as its whole stack; it replaces
    /// the stack `font` would give.
    pub custom_stack: Option<&'static str>,
    pub scale_bits: u32,
    pub zoom_bits: u32,
}

impl StyleEpoch {
    pub fn current(scale: f32) -> Self {
        Self {
            dark: theme::is_dark(),
            density: theme::density(),
            font: theme::terminal_code_font(),
            custom_stack: None,
            scale_bits: scale.to_bits(),
            zoom_bits: 1.0f32.to_bits(),
        }
    }
}

/// Per-terminal appearance. The host resolves System and validates settings;
/// this value never writes the shared app theme, code font or display zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalAppearance {
    pub dark: bool,
    pub font: FontChoice,
    /// See [`StyleEpoch::custom_stack`].
    pub custom_stack: Option<&'static str>,
    pub zoom: f32,
}

impl TerminalAppearance {
    pub(crate) fn epoch(self, scale: f32) -> StyleEpoch {
        StyleEpoch {
            dark: self.dark,
            font: self.font,
            custom_stack: self.custom_stack,
            zoom_bits: self.zoom.to_bits(),
            ..StyleEpoch::current(scale)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalStyle {
    pub epoch: StyleEpoch,
    pub metrics: TextMetrics,
    pub stack: &'static str,
    pub cell_width: f32,
    pub cell_height: f32,
}

impl TerminalStyle {
    /// **The default cell is measured at 100% display zoom.** Display zoom is
    /// a transform in both hosts — Koru Terminal's ADR-271 root scale, and
    /// koru.exe's whole-window `Affine::scale(dpi × zoom)` — and each of them
    /// already divides the layout viewport by it, so everything drawn in that
    /// space is multiplied back up exactly once. This used to read the
    /// `theme::zoom()` process global into the metrics as well, which was the
    /// pre-transform spelling of zoom: Koru Terminal had to pin the global at
    /// 1.0 to defuse it, and koru.exe — which keeps the global at the user's
    /// zoom for its indicator — got a grid at zoom² while the window around it
    /// was at zoom (Q-E8 P10). The global is a mirror for the indicator now,
    /// never a font size. Explicit Terminal appearance can independently scale
    /// text metrics; it never reads or duplicates that display transform.
    pub fn capture(engine: &mut koru_draw::text::TextEngine, scale: f32) -> Self {
        let epoch = StyleEpoch::current(scale);
        Self::capture_epoch(engine, epoch)
    }

    pub(crate) fn capture_epoch(
        engine: &mut koru_draw::text::TextEngine,
        epoch: StyleEpoch,
    ) -> Self {
        let text = theme::role::terminal();
        // This is terminal text zoom only. The host's display zoom is still
        // applied exactly once by its root transform, never read here.
        let zoom = f32::from_bits(epoch.zoom_bits);
        let metrics = TextMetrics::new(text.metrics.size * zoom, text.metrics.line_height * zoom);
        let stack = epoch.custom_stack.unwrap_or_else(|| epoch.font.stack());
        let cell_width = engine.measure_advance("M", stack, metrics.size, 400.0);
        Self {
            epoch,
            metrics,
            stack,
            cell_width,
            cell_height: metrics.line_height,
        }
    }

    pub fn resolve(self, style: CellStyle) -> ResolvedCellStyle {
        let palette = self.palette();
        let mut foreground = resolve_color(style.foreground, palette, self.epoch.dark);
        let mut background = resolve_color(style.background, palette, self.epoch.dark);
        if style.inverse {
            std::mem::swap(&mut foreground, &mut background);
        }
        if style.dim {
            foreground = theme::terminal_dim(foreground, background);
        }
        let face = match (style.bold, style.italic) {
            (false, false) => TerminalFace::Regular,
            (true, false) => TerminalFace::Bold,
            (false, true) => TerminalFace::Italic,
            (true, true) => TerminalFace::BoldItalic,
        };
        ResolvedCellStyle {
            foreground,
            background,
            face,
            underline: style.underline,
            strikeout: style.strikeout,
        }
    }

    pub fn palette(self) -> &'static theme::TerminalPalette {
        theme::terminal_palette_for(self.epoch.dark)
    }
}

/// **The terminal's background, and the only place it is read.**
///
/// Three things have to agree on one colour: the pane fill a host paints behind
/// the view, the fill the view paints across its own bounds, and every cell
/// whose background is `TerminalColor::DefaultBackground`. The agreement is
/// load-bearing rather than tidy, because a default-background cell now paints
/// *nothing* — it shows the ground under it, and if the ground were a different
/// colour every ordinary cell would be a box of the wrong shade. Reading one
/// function is what makes "the same colour" structural instead of a convention
/// three call sites happen to keep.
pub fn terminal_background() -> Color {
    theme::terminal_palette().background
}

fn resolve_color(color: TerminalColor, palette: &theme::TerminalPalette, dark: bool) -> Color {
    match color {
        TerminalColor::DefaultForeground => palette.foreground,
        TerminalColor::DefaultBackground => palette.background,
        TerminalColor::Indexed { index } => theme::terminal_indexed_for(index, dark),
        TerminalColor::Rgb { red, green, blue } => theme::terminal_rgb(red, green, blue),
    }
}
