// SPDX-License-Identifier: MPL-2.0

//! **How the pipeline is configured: the numbers Plan 0.1 measured.**
//!
//! Change them here and nowhere else, and change `MEASUREMENTS.md` with them.

/// ONNX Runtime intra-op threads, and the size of the pool `oar-ocr` does its
/// own pre- and post-processing on, when `KORU_READ_THREADS` says nothing.
/// *Measured, Plan 0.1*: above four there was little left to gain (scanned
/// pages 1.70 pages/s at 4, 1.80 at 8), and the worker must not take every
/// core of a machine a person is typing on.
pub const DEFAULT_THREADS: usize = 4;

/// The most threads a setting may ask for. A larger number is clamped, not
/// refused: it is a ceiling against a typo, not a judgement.
pub const MAX_THREADS: usize = 64;

/// **Text lines per recognizer call.** *Measured, Plan 0.1*: `oar-ocr`'s CPU
/// default of four pads every line in a call to the widest, which peaked at
/// 1,473 MiB in a long-lived process and was not faster; one line per call
/// peaked at 504 MiB a page and 594 MiB over 90 pages, inside the 1 GiB
/// ceiling the Store puts the worker under.
pub const REGION_BATCH: usize = 1;

/// **The detector's settings**: `oar-ocr`'s own PP-OCRv6 small preset
/// (`pp_ocrv6_detection_config`, which is private to that crate) and the
/// "general text" resize, written out so a change upstream cannot move them
/// under us. Plan 0.1 measured with exactly these.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Detection {
    /// A pixel is text above this probability.
    pub score_threshold: f32,
    /// A box is kept when its mean score is above this.
    pub box_threshold: f32,
    /// How far a box is grown around its text.
    pub unclip_ratio: f32,
    /// At most this many boxes per picture.
    pub max_candidates: usize,
    /// The picture's **longer** side is resized to at most this before
    /// detection. Recognition reads the crops from the full-size picture.
    pub max_side: u32,
}

/// The detector settings this reader runs with.
pub const DETECTION: Detection = Detection {
    score_threshold: 0.2,
    box_threshold: 0.45,
    unclip_ratio: 1.4,
    max_candidates: 3000,
    max_side: 960,
};

/// What a reader is built with, beyond its model files.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    threads: usize,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            threads: DEFAULT_THREADS,
        }
    }
}

impl Settings {
    /// `threads` clamped to `1..=MAX_THREADS`.
    pub fn with_threads(threads: usize) -> Settings {
        Settings {
            threads: threads.clamp(1, MAX_THREADS),
        }
    }

    /// From the text of `KORU_READ_THREADS`: unset or empty is the default; a
    /// whole number is clamped; anything else is the default and `Err` names
    /// it, so the caller can say so once instead of failing to start.
    pub fn from_setting(raw: Option<&str>) -> Result<Settings, Settings> {
        match raw.map(str::trim) {
            None | Some("") => Ok(Settings::default()),
            Some(text) => match text.parse::<usize>() {
                Ok(threads) => Ok(Settings::with_threads(threads)),
                Err(_) => Err(Settings::default()),
            },
        }
    }

    pub fn threads(&self) -> usize {
        self.threads
    }
}

#[cfg(test)]
mod tests;
