// SPDX-License-Identifier: MPL-2.0

//! **The model directory: three files, and a dictionary whose shape is
//! checked before anything trusts it.**
//!
//! ```text
//! <dir>/det.onnx       the text detector       (PP-OCRv6_small_det, converted)
//! <dir>/rec.onnx       the text recognizer     (PP-OCRv6_small_rec, converted)
//! <dir>/rec_dict.txt   the recognizer's alphabet, one character per line
//! ```
//!
//! The names are the conversion's (`scripts/dev/read-model/`, Plan 0.1), and
//! what Plan 2.4's installer places. The worker is told the directory once, at
//! start; it never searches for one.
//!
//! ## Why the dictionary is checked
//!
//! The recognizer answers a class index per time step, and the dictionary is
//! what turns an index into a character: line *n* is class *n + 1* (class 0 is
//! the CTC blank). `oar-ocr` reads it with `str::lines`, and **drops nothing,
//! guesses nothing**: an empty line in the middle becomes an empty
//! "character", a missing line shifts every index after it, and either way
//! every word read afterwards is wrong without any error. So a dictionary is
//! accepted only if every line is exactly one character, with no blank line
//! and no byte-order mark; one final newline is allowed. Plan 0.1's
//! conversion writes exactly that: 18,708 lines, one of them an ideographic
//! space.

use std::path::{Path, PathBuf};

/// The detector's file name.
pub const DETECTOR: &str = "det.onnx";
/// The recognizer's file name.
pub const RECOGNIZER: &str = "rec.onnx";
/// The dictionary's file name.
pub const DICTIONARY: &str = "rec_dict.txt";

/// A larger dictionary is not one of ours. PP-OCRv6's is 74,947 bytes.
pub const MAX_DICTIONARY_BYTES: u64 = 4 * 1024 * 1024;

/// A directory that should hold a model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelDir {
    dir: PathBuf,
}

/// Why a model directory cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelError {
    /// One of the three files is not there, or is not a file.
    Missing(&'static str),
    /// A file is there and could not be read.
    Unreadable(&'static str),
    /// The dictionary is not one character per line.
    Dictionary(DictionaryError),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Missing(name) => write!(f, "the model has no {name}"),
            ModelError::Unreadable(name) => write!(f, "the model's {name} could not be read"),
            ModelError::Dictionary(error) => write!(f, "the model's {DICTIONARY}: {error}"),
        }
    }
}

impl std::error::Error for ModelError {}

/// Why a dictionary is refused. Line numbers count from 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DictionaryError {
    Empty,
    /// It starts with a byte-order mark, which would become part of the first
    /// character.
    ByteOrderMark,
    /// A line with nothing on it.
    BlankLine(usize),
    /// A line with more than one character on it.
    NotOneCharacter(usize),
    TooLarge,
    NotUtf8,
}

impl std::fmt::Display for DictionaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DictionaryError::Empty => f.write_str("it is empty"),
            DictionaryError::ByteOrderMark => f.write_str("it starts with a byte-order mark"),
            DictionaryError::BlankLine(line) => write!(f, "line {line} is blank"),
            DictionaryError::NotOneCharacter(line) => {
                write!(f, "line {line} is not exactly one character")
            }
            DictionaryError::TooLarge => f.write_str("it is too large to be a dictionary"),
            DictionaryError::NotUtf8 => f.write_str("it is not UTF-8"),
        }
    }
}

/// Check a dictionary's text; answer how many characters it holds.
pub fn parse_dictionary(text: &str) -> Result<usize, DictionaryError> {
    if text.starts_with('\u{FEFF}') {
        return Err(DictionaryError::ByteOrderMark);
    }
    // One final newline is a file's ordinary end, not a blank line.
    let body = text.strip_suffix('\n').unwrap_or(text);
    if body.is_empty() {
        return Err(DictionaryError::Empty);
    }
    let mut count = 0;
    for (index, line) in body.split('\n').enumerate() {
        // `str::lines` — what `oar-ocr` reads with — takes "\r\n" as one end.
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut chars = line.chars();
        match (chars.next(), chars.next()) {
            (None, _) => return Err(DictionaryError::BlankLine(index + 1)),
            (Some(_), None) => count += 1,
            (Some(_), Some(_)) => return Err(DictionaryError::NotOneCharacter(index + 1)),
        }
    }
    Ok(count)
}

impl ModelDir {
    pub fn new(dir: impl Into<PathBuf>) -> ModelDir {
        ModelDir { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn detector(&self) -> PathBuf {
        self.dir.join(DETECTOR)
    }

    pub fn recognizer(&self) -> PathBuf {
        self.dir.join(RECOGNIZER)
    }

    pub fn dictionary(&self) -> PathBuf {
        self.dir.join(DICTIONARY)
    }

    /// **Whether the directory holds a usable model**, short of loading it:
    /// both model files are files, and the dictionary reads and has the right
    /// shape. Answers the dictionary's text, which is what the pipeline is then
    /// built from, so the bytes checked are the bytes used.
    ///
    /// Reads the disk: the worker calls it once at start and once before the
    /// first recognition, never on a request path.
    pub fn check(&self) -> Result<String, ModelError> {
        for (name, path) in [(DETECTOR, self.detector()), (RECOGNIZER, self.recognizer())] {
            if !path.is_file() {
                return Err(ModelError::Missing(name));
            }
        }
        let path = self.dictionary();
        let size = match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => meta.len(),
            Ok(_) | Err(_) => return Err(ModelError::Missing(DICTIONARY)),
        };
        if size > MAX_DICTIONARY_BYTES {
            return Err(ModelError::Dictionary(DictionaryError::TooLarge));
        }
        let bytes = std::fs::read(&path).map_err(|_| ModelError::Unreadable(DICTIONARY))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| ModelError::Dictionary(DictionaryError::NotUtf8))?;
        parse_dictionary(&text).map_err(ModelError::Dictionary)?;
        Ok(text)
    }
}

#[cfg(test)]
mod tests;
