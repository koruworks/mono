// SPDX-License-Identifier: MPL-2.0

//! **Recognized lines, into a page's text, in reading order — and how sure
//! the reader was.**
//!
//! The detector finds text lines as boxes, in an order of its own; the
//! recognizer reads each box into a string with a score. A page's text is
//! those strings in the order a person reads them: top to bottom, and left to
//! right within a line. Two boxes are on one line when their vertical centres
//! are within half a box height of each other (the smaller box's), so a
//! heading split into two boxes, or a price beside its label, stays on one
//! line; within a line boxes are joined by one space, and lines by `"\n"`.
//!
//! **Confidence is the mean of the recognizer's scores over the lines it
//! read** (each box is one recognized line), as a whole percentage. A page
//! with no words has no lines and answers 0, which is never written anywhere:
//! a page without words is progress, not a reading.
//!
//! Pure, so it is tested without a model.

use koru_read::{PageText, Recognized};

/// One recognized text line, with its box in picture pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub text: String,
    /// `0.0..=1.0`, or `None` when the recognizer gave none.
    pub confidence: Option<f32>,
}

impl Region {
    fn centre(&self) -> f32 {
        (self.top + self.bottom) / 2.0
    }

    fn height(&self) -> f32 {
        (self.bottom - self.top).max(1.0)
    }
}

/// Two boxes whose vertical centres are closer than this many heights (of the
/// smaller) are one line.
const SAME_LINE: f32 = 0.5;

/// The page's text and confidence from its recognized lines.
pub fn assemble(regions: Vec<Region>) -> Recognized {
    let mut regions: Vec<Region> = regions
        .into_iter()
        .filter(|region| {
            !region.text.trim().is_empty()
                && [region.left, region.top, region.right, region.bottom]
                    .iter()
                    .all(|value| value.is_finite())
        })
        .map(|mut region| {
            // A line is one line: whatever white space a recognizer put
            // inside it becomes single spaces.
            region.text = region.text.split_whitespace().collect::<Vec<_>>().join(" ");
            region
        })
        .collect();
    let confidence = mean_confidence(&regions);

    regions.sort_by(|a, b| a.centre().total_cmp(&b.centre()));
    let mut lines: Vec<Vec<Region>> = Vec::new();
    for region in regions {
        match lines.last_mut() {
            Some(line) if same_line(line, &region) => line.push(region),
            _ => lines.push(vec![region]),
        }
    }
    let text = lines
        .into_iter()
        .map(|mut line| {
            line.sort_by(|a, b| a.left.total_cmp(&b.left));
            line.into_iter()
                .map(|region| region.text)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    Recognized {
        text: PageText::new(text),
        confidence,
    }
}

fn same_line(line: &[Region], region: &Region) -> bool {
    let count = line.len() as f32;
    let centre = line.iter().map(Region::centre).sum::<f32>() / count;
    let height = line.iter().map(Region::height).sum::<f32>() / count;
    (region.centre() - centre).abs() <= SAME_LINE * height.min(region.height())
}

/// The mean score over the lines that have one, as `0..=100`; 0 with none.
pub fn mean_confidence(regions: &[Region]) -> u8 {
    let scores: Vec<f32> = regions
        .iter()
        .filter_map(|region| region.confidence)
        .filter(|score| score.is_finite())
        .collect();
    if scores.is_empty() {
        return 0;
    }
    let mean = scores.iter().sum::<f32>() / scores.len() as f32;
    (mean.clamp(0.0, 1.0) * 100.0).round() as u8
}

#[cfg(test)]
mod tests;
