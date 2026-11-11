// SPDX-License-Identifier: MPL-2.0

//! **A reading: the words found in one file's bytes, and what is known about
//! how they were found.**
//!
//! Every token here is stored in a synced document and read back by builds
//! older and newer than this one, so each has exactly one spelling, declared
//! once, and the same spelling crosses the worker's wire
//! (TECHNICAL §7 "Content", D8, D12).
//!
//! ## Tokens, never labels
//!
//! `page-cap`, not "Stopped at 200 pages". A surface looks up its own words in
//! `koru-i18n`; what is stored is what a reader branches on, in every language
//! at once.
//!
//! ## Unknown is not an error
//!
//! A newer build may write a status this one has never heard of. Parsing such a
//! token answers `None` rather than failing the document, because one entry a
//! build cannot understand must not make every other entry in the same
//! document unreadable.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// **The generation this build reads at** (D8).
///
/// Raised when a re-read is warranted: a better reader, a new script, word
/// positions for opening a file at the match. Nothing stored is ever migrated;
/// a raised number makes every older entry unfinished, and the coordinator
/// reads it again.
///
/// The rules that make two builds at different generations safe together:
///
/// - an entry at a generation **at or above** this one is finished business
///   for this build — it never reads it again and never writes over it
///   ([`crate::ReadingDoc::begin`] refuses);
/// - an entry **below** it is unfinished, and is read again at this one.
pub const GENERATION: u32 = 1;

/// At most this many pages of one PDF are sent to the reader, unless a person
/// asked for all of them (D12).
pub const READ_PAGE_CAP: u32 = 200;

/// At most this many pages of one PDF have their text layer taken, unless a
/// person asked for all of them (D12).
pub const TEXT_PAGE_CAP: u32 = 2_000;

/// Which reader produced a reading — `text` for a text layer or a plain-text
/// file, or the recognizer's own token.
///
/// **Checked, because it is synced.** Lowercase ASCII letters, digits and `-`;
/// non-empty; at most 32 characters. A token written into a document travels
/// to every device and every later build, so a value that is not a token is
/// refused on the way in rather than discovered on the way out.
///
/// This crate names only [`ReaderId::text`]. A recognizer's token belongs to
/// its adapter crate, which is the one place allowed to name what it adapts.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReaderId(String);

/// The token of the reader that is not a model: a PDF's text layer, or a
/// plain-text file decoded as it is.
pub const READER_TEXT: &str = "text";

impl ReaderId {
    /// Check and wrap. There is deliberately no unchecked constructor.
    pub fn parse(value: impl Into<String>) -> Option<ReaderId> {
        let value = value.into();
        let ok = !value.is_empty()
            && value.len() <= 32
            && value
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        ok.then_some(ReaderId(value))
    }

    /// The text-layer and plain-text reader, [`READER_TEXT`].
    pub fn text() -> ReaderId {
        ReaderId(READER_TEXT.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ReaderId {
    type Error = String;

    fn try_from(value: String) -> Result<ReaderId, String> {
        ReaderId::parse(value).ok_or_else(|| "a reader id is not a token".to_string())
    }
}

impl From<ReaderId> for String {
    fn from(value: ReaderId) -> String {
        value.0
    }
}

impl std::fmt::Display for ReaderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

tokens! {
    /// How a reading ended, or that it has not yet.
    ReadingStatus {
        /// Finished, with words.
        Read => "read",
        /// Finished, and there were no words. Synced, and not retried at this
        /// generation: a photograph of a beach is not going to grow text.
        Empty => "empty",
        /// Not finished. With [`Reason::PageCap`], it stopped at a cap; with
        /// no reason, it is still being read or is waiting for the reader.
        Partial => "partial",
        /// It cannot be read on any device, and [`Reason`] says why.
        Failed => "failed",
    }
}

tokens! {
    /// Why a reading is `failed` or `partial`.
    ///
    /// **Only outcomes that are true on every device are here** (TECHNICAL
    /// §7). A crash, a timeout or a format this computer cannot decode is a
    /// local fact — another device may succeed — and has no token in a synced
    /// document; it lives in the coordinator's local attempts table, under a
    /// [`crate::FailureCode`].
    Reason {
        /// With `failed`: the PDF is password-protected.
        Password => "password",
        /// With `failed`: the file is damaged beyond reading.
        Damaged => "damaged",
        /// With `partial`: a page cap was reached (D12).
        PageCap => "page-cap",
    }
}

tokens! {
    /// Where the words came from — for a whole reading, or for one page.
    Source {
        /// A PDF's own text layer.
        TextLayer => "text-layer",
        /// The reader, from pixels.
        Recognized => "recognized",
        /// Some pages one way and some the other. Only ever a whole reading's
        /// source; a page is never mixed.
        Mixed => "mixed",
        /// A plain-text file (`.txt`, `.md`, `.csv`, `.html` with its markup
        /// stripped). One page.
        Plain => "plain",
    }
}

impl ReadingStatus {
    /// Whether a reading in this state is done at its generation — read,
    /// empty, failed for good, or stopped at a cap. A `partial` with no reason
    /// is not: it is still being read, or waits for the reader.
    pub fn is_finished(self, reason: Option<Reason>) -> bool {
        match self {
            ReadingStatus::Read | ReadingStatus::Empty | ReadingStatus::Failed => true,
            ReadingStatus::Partial => reason == Some(Reason::PageCap),
        }
    }

    /// Whether `reason` may accompany this status. `failed` needs `password`
    /// or `damaged`; `partial` takes `page-cap` or nothing; `read` and `empty`
    /// take nothing.
    pub fn admits(self, reason: Option<Reason>) -> bool {
        matches!(
            (self, reason),
            (ReadingStatus::Read | ReadingStatus::Empty, None)
                | (
                    ReadingStatus::Failed,
                    Some(Reason::Password | Reason::Damaged)
                )
                | (ReadingStatus::Partial, None | Some(Reason::PageCap))
        )
    }
}

impl Source {
    /// The source of a reading made of pages from `self` and `other`.
    ///
    /// Text layer and recognition together are [`Source::Mixed`]; anything
    /// with itself is itself. A plain-text file has one page, so `Plain`
    /// meeting anything else is a caller's mistake and answers `Mixed` rather
    /// than inventing a precedence.
    pub fn combine(self, other: Source) -> Source {
        if self == other { self } else { Source::Mixed }
    }
}

/// **One page's words, wrapped so that printing them is impossible by
/// accident.**
///
/// `koru_voice::Transcript`'s idiom, for the same reason: a page of a person's
/// scanned contract passes through a worker, a pipe, a coordinator, a document
/// and a mirror, every one of which is a place somebody will one day add a
/// `{:?}`. `Debug` is redacted; `Serialize` is not and must not be — carrying
/// the words is the point of the pipe. There is no `Display`.
///
/// Lines are separated by `"\n"`, in reading order. The text is never
/// normalised: what the file held, or what the reader saw, byte for byte.
#[derive(Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PageText(String);

impl PageText {
    pub fn new(text: impl Into<String>) -> PageText {
        PageText(text.into())
    }

    /// The words, for a caller that is going to store or index them. A named
    /// method rather than a `Deref` so every use is a place a reviewer looks.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Length **in characters**, the only measurement of a page that may
    /// appear in a log or a diagnostic.
    pub fn chars(&self) -> usize {
        self.0.chars().count()
    }
}

impl std::fmt::Debug for PageText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PageText([REDACTED] chars={})", self.chars())
    }
}

impl From<&str> for PageText {
    fn from(text: &str) -> PageText {
        PageText::new(text)
    }
}

impl From<String> for PageText {
    fn from(text: String) -> PageText {
        PageText(text)
    }
}

/// **One blob's entry in a reading document**, as read back or as written whole.
///
/// The fields are the document's keys (TECHNICAL §7 "Content"). Everything that
/// a newer build might spell differently is an `Option`, because this value is
/// read from a document other builds write: `None` means absent, or a token
/// this build does not know.
///
/// `pages` holds only the pages of `generation`'s own map — see
/// [`crate::ReadingDoc`] on why a reading never mixes generations.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Reading {
    pub reader: Option<ReaderId>,
    pub generation: u32,
    pub status: Option<ReadingStatus>,
    pub reason: Option<Reason>,
    pub source: Option<Source>,
    /// `0..=100`; absent for text, which has no confidence to report.
    pub confidence: Option<u8>,
    /// With `partial`: how far the reading has got, so a restart resumes at
    /// the next page instead of the first.
    pub pages_done: Option<u32>,
    /// Page number (from 1) → its words. Keyed by number, so the same page
    /// written twice is one value.
    pub pages: BTreeMap<u32, PageText>,
}

impl Reading {
    /// Whether this reading is done at its own generation. See
    /// [`ReadingStatus::is_finished`]; an unknown status is not finished.
    pub fn is_finished(&self) -> bool {
        self.status.is_some_and(|s| s.is_finished(self.reason))
    }

    /// Whether this build has nothing left to do with it: it is finished at
    /// [`GENERATION`], or was written at a higher one and a newer build owns
    /// it. The coordinator's "has no finished entry at the current generation"
    /// is the negation of this.
    pub fn is_settled_for_this_build(&self) -> bool {
        self.generation > GENERATION || (self.generation == GENERATION && self.is_finished())
    }
}

#[cfg(test)]
mod tests;
