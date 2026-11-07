// SPDX-License-Identifier: MPL-2.0

//! **KoruLauncher — "what do you want to make?", in the middle of the window.**
//!
//! A grid of tiles on a popover, each an icon over a word with one letter of
//! it underlined: that letter chooses the tile. The launcher knows nothing of
//! what a tile makes; the host hands it the tiles and is answered with which
//! one was chosen, and with whether Alt (ask where it opens) or Shift (ask
//! which kind) was held.
//!
//! Consumer: the window's *New…* — the tab strip's `+`, `Ctrl+N` and the
//! explore lane's *New*.
//!
//! **Composed, not drawn anew.** The surface is [`PopoverShell`]; a tile's
//! lit plate is the hover tint every row and button in the app lights with.
//!
//! | | |
//! | --- | --- |
//! | Geometry | Centred in the window, up to [`COLUMNS`] tiles a row. Draw, hit-test and the semantics' rects all come from [`Launcher::panel`] and [`Launcher::tile_rect`]. |
//! | Keys | A tile's letter chooses it, with the modifiers held (case-blind, so Shift+S is S with Shift). The arrows move the highlight across the grid; Enter or Space chooses it; Escape dismisses. Every other key is swallowed ([`Outcome::Handled`]): it is modal while it stands. |
//! | Pointer | Hovering a tile highlights it; a press on a tile chooses it with the modifiers held; a press anywhere else dismisses. The pointer is [`cursor::PRESS`](crate::cursor::PRESS) over a tile — never the hand (ADR-215). |
//! | Accessibility | [`Launcher::semantics`]: a menu named by the host, one item per tile named by its word; the highlight is the published focus. |
//! | Words | The host's, already translated — the name and every tile's word. |

use std::sync::atomic::{AtomicU64, Ordering};

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{inspect, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key as KeyPress, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use crate::PopoverShell;

#[cfg(test)]
mod tests;

/// The most tiles a row holds.
pub const COLUMNS: usize = 3;
/// A tile's size.
pub const TILE_W: f32 = 132.0;
pub const TILE_H: f32 = 96.0;
/// Between tiles.
pub const GAP: f32 = 8.0;
/// The tile's glyph.
const ICON: f32 = 22.0;
/// The lit plate's corner.
const RADIUS: f32 = 10.0;
/// The footer a hint sits in, under the tiles.
pub const HINT_H: f32 = 30.0;

/// One tile: its glyph, its word, and the letter that chooses it.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub glyph: char,
    pub label: String,
    /// Lowercase. Underlined where it first appears in `label`; a word in a
    /// language that lacks the letter is still chosen by it.
    pub letter: char,
}

/// What a press or a key decided.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Tile `index` was chosen; `alt` asks where it opens, `shift` which kind.
    Chose {
        index: usize,
        alt: bool,
        shift: bool,
    },
    /// The highlight moved.
    Moved(usize),
    /// Escape, or a press off the tiles.
    Dismissed,
    /// Swallowed.
    Handled,
}

/// What a screen reader hears.
#[derive(Clone, Debug, PartialEq)]
pub struct Semantics {
    pub name: String,
    pub bounds: Rect,
    pub items: Vec<SemanticItem>,
    pub focus: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticItem {
    pub index: usize,
    pub label: String,
    pub rect: Rect,
    pub instance: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Launcher {
    name: String,
    tiles: Vec<Tile>,
    /// One muted line under the tiles saying what the modifiers do.
    hint: Option<String>,
    highlight: usize,
    instance: u64,
}

impl Launcher {
    /// Open, named `name`, over `tiles`, highlighting the first.
    pub fn open(name: impl Into<String>, tiles: Vec<Tile>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let count = tiles.len() as u64;
        Self {
            name: name.into(),
            tiles,
            hint: None,
            highlight: 0,
            instance: NEXT.fetch_add(count + 1, Ordering::Relaxed),
        }
    }

    /// A footer line under the tiles — what holding a modifier adds.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    pub fn tiles(&self) -> &[Tile] {
        &self.tiles
    }

    pub fn highlight(&self) -> usize {
        self.highlight
    }

    pub fn set_highlight(&mut self, index: usize) {
        if index < self.tiles.len() {
            self.highlight = index;
        }
    }

    /// A block of ids for an accessibility tree: this, then one per tile.
    pub fn instance(&self) -> u64 {
        self.instance
    }

    pub fn tile_instance(&self, index: usize) -> u64 {
        self.instance + 1 + index as u64
    }

    pub fn tile_of_instance(&self, id: u64) -> Option<usize> {
        let offset = usize::try_from(id.checked_sub(self.instance + 1)?).ok()?;
        (offset < self.tiles.len()).then_some(offset)
    }

    fn columns(&self) -> usize {
        self.tiles.len().clamp(1, COLUMNS)
    }

    fn rows(&self) -> usize {
        self.tiles.len().div_ceil(self.columns()).max(1)
    }

    /// The surface, centred in the window.
    pub fn panel(&self, viewport: (f32, f32)) -> Rect {
        let pad = theme::PANEL_PAD;
        let rows = self.rows() as f32;
        // A hint needs the room of a full row of tiles, however few there
        // are; the grid is centred in what is left over.
        let cols = if self.hint.is_some() {
            COLUMNS
        } else {
            self.columns()
        } as f32;
        let w = pad * 2.0 + cols * TILE_W + (cols - 1.0) * GAP;
        let footer = if self.hint.is_some() { HINT_H } else { 0.0 };
        let h = pad * 2.0 + rows * TILE_H + (rows - 1.0) * GAP + footer;
        Rect::new(
            ((viewport.0 - w) * 0.5).max(0.0),
            ((viewport.1 - h) * 0.5).max(0.0),
            w,
            h,
        )
    }

    /// Where tile `index` is drawn and hit.
    pub fn tile_rect(&self, viewport: (f32, f32), index: usize) -> Rect {
        let panel = self.panel(viewport);
        let cols = self.columns();
        let grid_w = cols as f32 * TILE_W + (cols as f32 - 1.0) * GAP;
        let left = panel.x + (panel.w - grid_w) * 0.5;
        let (col, row) = ((index % cols) as f32, (index / cols) as f32);
        Rect::new(
            left + col * (TILE_W + GAP),
            panel.y + theme::PANEL_PAD + row * (TILE_H + GAP),
            TILE_W,
            TILE_H,
        )
    }

    /// The footer under the tiles, when there is a hint to put in it.
    pub fn hint_rect(&self, viewport: (f32, f32)) -> Option<Rect> {
        self.hint.as_ref()?;
        let panel = self.panel(viewport);
        let y = panel.y + panel.h - theme::PANEL_PAD - HINT_H;
        Some(Rect::new(
            panel.x + theme::PANEL_PAD,
            y,
            panel.w - theme::PANEL_PAD * 2.0,
            HINT_H,
        ))
    }

    pub fn tile_at(&self, viewport: (f32, f32), x: f32, y: f32) -> Option<usize> {
        (0..self.tiles.len()).find(|&i| self.tile_rect(viewport, i).contains(x, y))
    }

    /// A press: a tile chooses itself with the modifiers held; anywhere else
    /// dismisses.
    pub fn mouse_down(
        &self,
        viewport: (f32, f32),
        x: f32,
        y: f32,
        mods: ModifiersState,
    ) -> Outcome {
        match self.tile_at(viewport, x, y) {
            Some(index) => Outcome::Chose {
                index,
                alt: mods.alt_key(),
                shift: mods.shift_key(),
            },
            None => Outcome::Dismissed,
        }
    }

    /// The tile under the pointer is the highlight. `true` when it changed.
    pub fn mouse_move(&mut self, viewport: (f32, f32), x: f32, y: f32) -> bool {
        match self.tile_at(viewport, x, y) {
            Some(index) if index != self.highlight => {
                self.highlight = index;
                true
            }
            _ => false,
        }
    }

    /// A key, with the modifiers held.
    pub fn key(&mut self, key: &KeyPress, mods: ModifiersState) -> Outcome {
        let chose = |index| Outcome::Chose {
            index,
            alt: mods.alt_key(),
            shift: mods.shift_key(),
        };
        match key {
            KeyPress::Named(NamedKey::Escape) => Outcome::Dismissed,
            KeyPress::Named(NamedKey::Enter | NamedKey::Space) => chose(self.highlight),
            KeyPress::Named(
                arrow @ (NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown),
            ) => {
                self.highlight = self.step(*arrow);
                Outcome::Moved(self.highlight)
            }
            KeyPress::Character(c) if c.as_str() == " " => chose(self.highlight),
            KeyPress::Character(c) => {
                let typed = c.chars().next().map(|c| c.to_ascii_lowercase());
                match self
                    .tiles
                    .iter()
                    .position(|tile| Some(tile.letter.to_ascii_lowercase()) == typed)
                {
                    Some(index) => chose(index),
                    None => Outcome::Handled,
                }
            }
            _ => Outcome::Handled,
        }
    }

    /// The highlight one step in `arrow`'s direction across the grid,
    /// wrapping at its ends.
    fn step(&self, arrow: NamedKey) -> usize {
        let n = self.tiles.len().max(1);
        let cols = self.columns();
        let i = self.highlight;
        match arrow {
            NamedKey::ArrowRight => (i + 1) % n,
            NamedKey::ArrowLeft => (i + n - 1) % n,
            NamedKey::ArrowDown => {
                let next = i + cols;
                if next < n { next } else { i % cols }
            }
            NamedKey::ArrowUp => {
                if i >= cols {
                    i - cols
                } else {
                    let last_row = (n - 1) / cols;
                    (last_row * cols + i).min(n - 1)
                }
            }
            _ => i,
        }
    }

    pub fn cursor(&self, viewport: (f32, f32), x: f32, y: f32) -> CursorIcon {
        match self.tile_at(viewport, x, y) {
            Some(_) => crate::cursor::PRESS,
            None => crate::cursor::IDLE,
        }
    }

    pub fn semantics(&self, viewport: (f32, f32)) -> Semantics {
        Semantics {
            name: self.name.clone(),
            bounds: self.panel(viewport),
            items: self
                .tiles
                .iter()
                .enumerate()
                .map(|(index, tile)| SemanticItem {
                    index,
                    label: tile.label.clone(),
                    rect: self.tile_rect(viewport, index),
                    instance: self.tile_instance(index),
                })
                .collect(),
            focus: self.highlight,
        }
    }

    /// Paint the launcher: the popover and its tiles, the highlighted one lit.
    #[track_caller]
    pub fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
    ) {
        let panel = self.panel(viewport);
        let _component = inspect::scope(
            "KoruLauncher",
            "component",
            root,
            scene::rect(panel.x, panel.y, panel.w, panel.h),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::launcher::Launcher",
                    "shared implementation",
                );
                r.property("tiles", self.tiles.len(), "instance");
                r.property("highlight", self.highlight, "instance state");
                r.property("hint", self.hint.as_deref().unwrap_or(""), "instance");
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        let shell = PopoverShell {
            rect: panel,
            radius: theme::overlay::popover().radius,
        };
        let p = theme::p();
        shell.draw_containing(canvas, root, |canvas, root| {
            for (index, tile) in self.tiles.iter().enumerate() {
                let r = self.tile_rect(viewport, index);
                if index == self.highlight {
                    scene::fill(
                        canvas,
                        root,
                        p.hover_tint,
                        &scene::rrect(r.x, r.y, r.w, r.h, RADIUS),
                    );
                }
                let icon = engine.icon_layout(tile.glyph, ICON);
                let label = engine.simple_layout(&tile.label, theme::role::row());
                let label_h = scene::first_line_height(&label);
                let block = icon.height() + 10.0 + label_h;
                let top = r.y + (r.h - block) * 0.5;
                scene::draw_layout(
                    canvas,
                    root,
                    &icon,
                    r.x + (r.w - icon.width()) * 0.5,
                    top,
                    Some(p.text),
                );
                let lx = r.x + (r.w - label.width()) * 0.5;
                let ly = top + icon.height() + 10.0;
                scene::draw_layout_first_line(canvas, root, &label, lx, ly, Some(p.text));
                // The letter that chooses it, underlined in the accent.
                if let Some((before, letter)) = letter_span(&tile.label, tile.letter) {
                    let x0 = lx + engine.simple_layout(before, theme::role::row()).width();
                    let w = engine.simple_layout(letter, theme::role::row()).width();
                    let y = (ly + label_h - 3.0).round();
                    scene::fill(canvas, root, p.accent, &scene::rect(x0, y, w, 1.5));
                }
            }
            // The footer: what holding a modifier adds, muted and centred.
            if let (Some(hint), Some(r)) = (self.hint.as_deref(), self.hint_rect(viewport)) {
                let layout = engine.simple_layout(hint, theme::role::hint());
                let h = scene::first_line_height(&layout);
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &layout,
                    r.x + ((r.w - layout.width()) * 0.5).max(0.0),
                    (r.y + (r.h - h) * 0.5).round(),
                    Some(p.text_dim),
                );
            }
        });
    }
}

/// The word before the first appearance of `letter` (case-blind) and the
/// letter as written, or `None` when the word lacks it.
fn letter_span(label: &str, letter: char) -> Option<(&str, &str)> {
    let (at, c) = label
        .char_indices()
        .find(|(_, c)| c.to_lowercase().eq(letter.to_lowercase()))?;
    Some((&label[..at], &label[at..at + c.len_utf8()]))
}
