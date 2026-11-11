// SPDX-License-Identifier: MPL-2.0

//! **Whether a page's text layer looks like text** (TECHNICAL §8).
//!
//! A PDF page may carry real text, no text at all, a scan with an invisible
//! layer that a previous OCR put there, or glyphs in a font that says nothing
//! about which characters they are. The last kind extracts as nothing useful
//! and must be *read*, from pixels, instead. So a page's text layer is taken
//! only when it has at least a handful of characters and few of its glyphs
//! are unmapped; otherwise the page is left for the reader.
//!
//! ## Where the numbers come from
//!
//! The character and unmapped thresholds are **measured** (OCR roadmap Plan
//! 0.1, 721 real PDFs, 9,089 pages, `hayro` 0.9): accept a text layer with at
//! least 20 non-white-space characters and at most 10% of its glyphs
//! unmapped. Glyphs mapped to private-use or control code points — icon fonts,
//! bullets — count neither as characters nor against the page (the text
//! device drops them). The picture share is not measured and is this plan's
//! own guess. Change them here and nowhere else.

/// What the text device found on one page, before any decision.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layer {
    /// The page's words, lines in reading order separated by `"\n"`.
    pub text: String,
    /// Glyphs that carried a Unicode mapping and are not white space.
    pub chars: usize,
    /// Glyphs the font could not map to any character.
    pub unmapped: usize,
    /// The share of the page, `0.0..=1.0`, covered by drawn pictures (summed,
    /// so overlapping pictures can exceed what is visible; capped at 1).
    pub picture_share: f64,
}

/// At least this many mapped, non-space characters make a text layer worth
/// taking. *Measured, Plan 0.1.*
pub const MIN_TEXT_CHARS: usize = 20;

/// More than this share of unmapped glyphs (in thousandths) means the text
/// layer is not trustworthy and the page needs the reader. *Measured, Plan
/// 0.1.*
pub const MAX_UNMAPPED_PERMILLE: usize = 100;

/// A page whose pictures cover at least this share of it, and whose text
/// layer is short, is treated as a scan. A small logo beside a page number is
/// not. *Not measured: this plan's guess.*
pub const MIN_PICTURE_SHARE: f64 = 0.10;

/// What to do with one page.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Take the text layer: it is the page's words.
    Text,
    /// The page holds no words and nothing that could be read for them.
    Nothing,
    /// The text layer cannot be trusted or is missing over a picture: the
    /// page has to be rendered and read.
    NeedsReader,
}

/// Judge one page.
pub fn judge(layer: &Layer) -> Verdict {
    let glyphs = layer.chars + layer.unmapped;
    if layer.unmapped * 1000 > MAX_UNMAPPED_PERMILLE * glyphs {
        return Verdict::NeedsReader;
    }
    if layer.chars >= MIN_TEXT_CHARS {
        return Verdict::Text;
    }
    // Short text, or none. Over a picture it is a scan (perhaps with a page
    // number in a real font). **Without a picture the page really is that
    // short** — a blank page, a title, a page number — and reading its pixels
    // would find the same: so it is taken as it is rather than making a text
    // PDF wait for a download it does not need. (TECHNICAL §8 says a page that
    // fails the check "is treated as a scan"; this is the one place the worker
    // is narrower, and why.)
    if layer.picture_share >= MIN_PICTURE_SHARE {
        return Verdict::NeedsReader;
    }
    if layer.chars == 0 {
        Verdict::Nothing
    } else {
        Verdict::Text
    }
}

#[cfg(test)]
mod tests;
