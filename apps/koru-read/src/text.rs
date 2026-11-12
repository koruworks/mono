// SPDX-License-Identifier: MPL-2.0

//! **A page's glyphs, collected into lines in reading order.**
//!
//! `hayro-interpret` has no "give me this page's text" call (TECHNICAL §8). It
//! interprets a page's content and hands every run of positioned glyphs, each
//! with whatever Unicode mapping its font provides, to a `Device` the caller
//! implements. [`Collector`] is that device: it keeps each glyph's position,
//! size and characters and ignores every drawing operation except pictures,
//! whose share of the page [`crate::accept`] needs. [`assemble`] then turns
//! the glyphs into lines.
//!
//! ## How lines are found
//!
//! Content streams draw text in any order, so the order glyphs arrive in is
//! not the reading order. Each glyph is placed in its own baseline frame — `u`
//! along the baseline, `v` towards the top of the glyph — so text turned a
//! quarter, a half or three quarters reads the same way upright text does.
//! Within one orientation:
//!
//! 1. glyphs are sorted top to bottom and clustered into a line while their
//!    baselines are within half a glyph height of each other (so a superscript
//!    stays on its line);
//! 2. each line is sorted left to right, and a space is inserted where the gap
//!    after a glyph is wider than a fifth of an em — PDFs very often position
//!    words apart instead of drawing a space;
//! 3. a glyph drawn twice in the same place (text that is filled and then
//!    stroked, or "bold" made by overprinting) is kept once.
//!
//! Upright text comes first, then text turned left, turned right, upside down,
//! and last any text set in vertical writing mode (columns read right to
//! left). **Columns side by side are read across**, line by line: a page's
//! words are all there and in order within each line, which is what search,
//! one fragment per page, needs; reading a two-column article column by column
//! is not attempted.
//!
//! ## What counts as unmapped
//!
//! A glyph with no Unicode (or only the replacement character) is counted and
//! dropped: its count is what tells a real text layer from one whose font
//! says nothing about itself. A glyph mapped only to control or private-use
//! code points — an icon font, a bullet — is dropped and **not** counted,
//! either way (Plan 0.1 measured that these are ornaments, not missing text).
//!
//! Whitespace mapped by a font is a character too; it reaches [`assemble`]
//! and becomes a word break.

use hayro_interpret::font::{Glyph, GlyphRun};
use hayro_interpret::hayro_cmap::{BfString, WritingMode};
use hayro_interpret::kurbo::{Affine, BezPath, Point, Rect};
use hayro_interpret::{
    BlendMode, ClipPath, Device, DrawMode, DrawProps, Image, ImageDrawProps, SoftMask,
};

use crate::accept::Layer;

/// Glyph space is 1000 units to the em in `hayro-font` (`UNITS_PER_EM`), and
/// every glyph transform it hands over maps that space to the page.
const UNITS_PER_EM: f64 = 1000.0;

/// Two baselines closer than this many glyph heights are one line.
const LINE_TOLERANCE_EM: f64 = 0.5;

/// A gap wider than this many ems between two glyphs is a word break.
const SPACE_GAP_EM: f64 = 0.2;

/// The same glyph closer than this many ems to the previous one is the same
/// glyph drawn twice.
const DUPLICATE_EM: f64 = 0.1;

/// Below this size (page units) a glyph height is treated as this, so a
/// degenerate transform cannot make every tolerance zero.
const MIN_SIZE: f64 = 1.0;

/// A page with more glyphs than this keeps only the first so many. A real
/// page has a few thousand; this bounds what a hostile one can make the
/// worker hold before the memory ceiling has to.
pub const MAX_GLYPHS_PER_PAGE: usize = 250_000;

/// Which way a glyph's baseline runs.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Orientation {
    Upright,
    /// Baseline running up the page (turned a quarter to the left).
    Left,
    UpsideDown,
    /// Baseline running down the page.
    Right,
    /// Vertical writing mode: glyphs upright, advancing down, columns right
    /// to left.
    Vertical,
}

/// The order orientations are read in.
const READING: [Orientation; 5] = [
    Orientation::Upright,
    Orientation::Left,
    Orientation::Right,
    Orientation::UpsideDown,
    Orientation::Vertical,
];

/// One glyph, in its own baseline frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub orientation: Orientation,
    /// Along the baseline, in reading direction.
    pub u: f64,
    /// Towards the top of the glyph: a larger `v` is an earlier line.
    pub v: f64,
    /// How far along `u` the glyph advances.
    pub advance: f64,
    /// The glyph's height (its font size on the page).
    pub size: f64,
    pub text: String,
}

/// Turn glyphs into the page's text: lines in reading order, separated by
/// `"\n"`, each with single spaces between words and no space at either end.
pub fn assemble(glyphs: &[Placed]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for orientation in READING {
        let mut group: Vec<&Placed> = glyphs
            .iter()
            .filter(|glyph| glyph.orientation == orientation)
            .collect();
        // Stable, so glyphs at the same place keep the order they were drawn.
        group.sort_by(|a, b| b.v.total_cmp(&a.v));
        let mut clusters: Vec<(f64, f64, Vec<&Placed>)> = Vec::new();
        for glyph in group {
            let size = glyph.size.max(MIN_SIZE);
            match clusters.last_mut() {
                Some((line_v, line_size, members))
                    if (*line_v - glyph.v).abs() <= LINE_TOLERANCE_EM * size.min(*line_size) =>
                {
                    members.push(glyph);
                }
                _ => clusters.push((glyph.v, size, vec![glyph])),
            }
        }
        for (_, _, mut members) in clusters {
            members.sort_by(|a, b| a.u.total_cmp(&b.u));
            let line = join(&members);
            if !line.is_empty() {
                lines.push(line);
            }
        }
    }
    lines.join("\n")
}

/// One line's glyphs, left to right, into words.
fn join(glyphs: &[&Placed]) -> String {
    let mut out = String::new();
    let mut previous: Option<&Placed> = None;
    for glyph in glyphs {
        if let Some(before) = previous {
            let em = before.size.max(glyph.size).max(MIN_SIZE);
            if glyph.text == before.text && (glyph.u - before.u).abs() < DUPLICATE_EM * em {
                continue;
            }
            let gap = glyph.u - (before.u + before.advance);
            if gap > SPACE_GAP_EM * em {
                out.push(' ');
            }
        }
        out.push_str(&glyph.text);
        previous = Some(glyph);
    }
    // Collapse whatever white space the glyphs and the gaps produced.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What a glyph's mapping says about it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Mapping {
    /// Characters a person could search for.
    Text,
    /// No mapping at all, or only the replacement character: the font does
    /// not say what this glyph is. Counted against the page.
    Unmapped,
    /// Only private-use or control code points: an icon font, a bullet.
    /// Present but meaningless — neither a character nor held against the
    /// page (Plan 0.1's measurement).
    Ornament,
}

/// A character a person could type into search.
fn searchable(c: char) -> bool {
    !c.is_control()
        && c != char::REPLACEMENT_CHARACTER
        && !matches!(c, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{10FFFF}')
}

fn mapping(text: &str) -> Mapping {
    if text.chars().all(|c| c == char::REPLACEMENT_CHARACTER) {
        Mapping::Unmapped
    } else if text.chars().any(searchable) {
        Mapping::Text
    } else {
        Mapping::Ornament
    }
}

/// **The device a page is interpreted into.** It draws nothing.
pub struct Collector {
    glyphs: Vec<Placed>,
    unmapped: usize,
    picture_area: f64,
    page_area: f64,
    largest_image: u64,
    image_pixels: u64,
}

impl Collector {
    /// `page_area` is the page's area in the same units the page transform
    /// produces, for the picture share.
    pub fn new(page_area: f64) -> Collector {
        Collector {
            glyphs: Vec::new(),
            unmapped: 0,
            picture_area: 0.0,
            page_area,
            largest_image: 0,
            image_pixels: 0,
        }
    }

    /// What the page held.
    pub fn finish(self) -> Layer {
        let text = assemble(&self.glyphs);
        let chars = text.chars().filter(|c| !c.is_whitespace()).count();
        let picture_share = if self.page_area > 0.0 && self.page_area.is_finite() {
            (self.picture_area / self.page_area).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Layer {
            text,
            chars,
            unmapped: self.unmapped,
            picture_share,
            largest_image: self.largest_image,
            image_pixels: self.image_pixels,
        }
    }

    /// Record one glyph drawn with `transform` (page space from glyph space).
    fn place(
        &mut self,
        transform: Affine,
        advance_units: Option<f32>,
        writing: WritingMode,
        text: Option<BfString>,
    ) {
        let text = match text {
            Some(BfString::Char(c)) => c.to_string(),
            Some(BfString::String(s)) => s,
            None => String::new(),
        };
        match mapping(&text) {
            Mapping::Text => {}
            Mapping::Unmapped => {
                self.unmapped += 1;
                return;
            }
            Mapping::Ornament => return,
        }
        // Keep only what can be searched for: a control or private-use code
        // point mixed into a mapping goes.
        let text: String = text.chars().filter(|&c| searchable(c)).collect();
        if self.glyphs.len() >= MAX_GLYPHS_PER_PAGE {
            return;
        }
        let origin = transform * Point::ZERO;
        let along = transform * Point::new(UNITS_PER_EM, 0.0) - origin;
        let up = transform * Point::new(0.0, UNITS_PER_EM) - origin;
        let size = up.hypot();
        if !origin.x.is_finite() || !origin.y.is_finite() || !size.is_finite() {
            self.unmapped += 1;
            return;
        }
        let orientation = if matches!(writing, WritingMode::Vertical) {
            Orientation::Vertical
        } else {
            let quarter = (along.y.atan2(along.x) / std::f64::consts::FRAC_PI_2).round() as i64;
            match quarter.rem_euclid(4) {
                0 => Orientation::Upright,
                1 => Orientation::Left,
                2 => Orientation::UpsideDown,
                _ => Orientation::Right,
            }
        };
        let frame = |point: Point| -> (f64, f64) {
            match orientation {
                Orientation::Upright => (point.x, point.y),
                Orientation::Left => (point.y, -point.x),
                Orientation::UpsideDown => (-point.x, -point.y),
                Orientation::Right => (-point.y, point.x),
                Orientation::Vertical => (-point.y, point.x),
            }
        };
        let (u, v) = frame(origin);
        let advance = match (orientation, advance_units) {
            // Vertical writing advances by roughly an em; the font's vertical
            // metrics are not exposed, and an em is what keeps columns apart.
            (Orientation::Vertical, _) => size,
            (_, Some(units)) if units.is_finite() && units > 0.0 => {
                let end = transform * Point::new(f64::from(units), 0.0);
                (frame(end).0 - u).abs()
            }
            // Unknown width: half an em, the width of an average letter.
            _ => size * 0.5,
        };
        self.glyphs.push(Placed {
            orientation,
            u,
            v,
            advance,
            size,
            text,
        });
    }
}

impl<'a> Device<'a> for Collector {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

    /// Every glyph, visible or not: an invisible text layer laid over a scan
    /// is exactly the text a previous OCR left there.
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        let writing = run.writing_mode();
        for glyph in run.glyphs() {
            let advance = match &**glyph {
                Glyph::Outline(outline) => outline.advance_width(),
                Glyph::Type3(type3) => type3.advance_width(),
            };
            self.place(
                props.transform * glyph.transform(),
                advance,
                writing,
                glyph.as_unicode(),
            );
        }
    }

    /// A picture's area on the page. `hayro` hands the transform from the
    /// picture's *pixel* grid (as `hayro` 0.9.0's `x_object/image.rs` builds
    /// it, and the tests hold), so the area is that transform's scale times
    /// the pixel count. `width`/`height` are public but hidden from its docs;
    /// if they go, this is where the fixture test fails.
    ///
    /// The same declared size is what rendering the page would make `hayro`
    /// decode — it decodes every image at full size — so it is recorded here,
    /// where nothing has been decoded yet, for the bound in
    /// [`crate::picture::page_images_allowed`].
    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
        let declared = u64::from(image.width()) * u64::from(image.height());
        self.largest_image = self.largest_image.max(declared);
        self.image_pixels = self.image_pixels.saturating_add(declared);
        let pixels = f64::from(image.width()) * f64::from(image.height());
        let area = props.transform.determinant().abs() * pixels;
        if area.is_finite() {
            self.picture_area += area;
        }
    }

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}

    fn draw_rect(&mut self, _: &Rect, _: DrawProps<'a>, _: &DrawMode) {}
}

#[cfg(test)]
mod tests;
