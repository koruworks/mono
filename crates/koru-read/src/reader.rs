// SPDX-License-Identifier: MPL-2.0

//! **What a reader is asked, and what it answers** (TECHNICAL D5).
//!
//! A reader turns a picture into words. That is all: the worker opens the file,
//! parses a PDF, renders a scanned page, applies a photograph's EXIF rotation
//! and scales a very large picture down, and hands the reader pixels. So the
//! reader never sees a path, a PDF or an encoded image, and an adapter is the
//! runtime and nothing else.
//!
//! Nothing here implements it. `koru-read-paddle` does, behind a `runtime`
//! feature only the worker enables, so `cargo test --workspace` never compiles
//! an inference runtime. A second reader — a script the first does not read, a
//! document-parsing model, the system's own reader where it is clearly better —
//! is a second implementation of this trait, and nothing above it changes.
//!
//! ## The thread contract
//!
//! `Send` and not `Sync`, as `koru_voice::SpeechEngine` is: the worker moves
//! the reader onto its one reading thread and never shares it, so an adapter
//! over a native handle that is not thread-safe needs no lock. [`Reader::read`]
//! blocks for as long as recognition takes — the worker owns the time budget
//! and ends the process when a page exceeds it, which is the only way to stop
//! a native call that will not return.

use crate::reading::{PageText, ReaderId};
use crate::wire::FailureCode;

/// **A decoded picture**: tightly packed 8-bit RGB, row-major, top row first,
/// `width * height * 3` bytes, already upright.
///
/// One pixel format, because one is all a reader needs and every decoder can
/// produce it; a reader that wants grey converts, in one place, rather than
/// every caller guessing which format a given reader accepts.
#[derive(Clone, Copy, Debug)]
pub struct Picture<'a> {
    width: u32,
    height: u32,
    pixels: &'a [u8],
}

impl<'a> Picture<'a> {
    /// Check and wrap. `None` when either side is zero or `pixels` is not
    /// exactly `width * height * 3` bytes.
    pub fn rgb8(width: u32, height: u32, pixels: &'a [u8]) -> Option<Picture<'a>> {
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(3)?;
        (width > 0 && height > 0 && pixels.len() == expected).then_some(Picture {
            width,
            height,
            pixels,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &'a [u8] {
        self.pixels
    }
}

/// What a reader found in one picture.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Recognized {
    /// Lines in reading order, separated by `"\n"`. Empty when there were no
    /// words, which is an answer, not a failure.
    pub text: PageText,
    /// `0..=100`. Stored so a low-confidence reading can be found again when a
    /// better reader exists.
    pub confidence: u8,
}

/// What a reader says about itself.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReaderInfo {
    /// The token written into every reading it produces.
    pub reader: ReaderId,
    /// The runtime's and model's own version string. Printed and logged,
    /// **never parsed and never compared**: what decides a re-read is
    /// [`crate::GENERATION`], not this.
    pub version: String,
}

/// A reader that could not answer. `code` is branched on; `message` is the
/// runtime's own developer text, never shown to a person and **never given a
/// word of the picture**.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReadError {
    pub code: FailureCode,
    pub message: String,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ReadError {}

/// **A reader, from the outside.**
pub trait Reader: Send {
    /// What this is.
    fn describe(&self) -> ReaderInfo;

    /// Read one upright picture.
    fn read(&mut self, picture: &Picture<'_>) -> Result<Recognized, ReadError>;
}

#[cfg(test)]
mod tests;
