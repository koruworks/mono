// SPDX-License-Identifier: MPL-2.0

//! **The reading document's Loro shape, and the only code that writes it.**
//!
//! One per catalog entry that references a readable blob: a file, or a page
//! that holds pictures (TECHNICAL D9). It is **not** a `WorkspaceDoc` and holds
//! no node tree; its only root is a map.
//!
//! ```text
//! readings          the document's only root: doc.get_map("readings")
//!   <b3:hex>        a mergeable map, one per blob the parent references.
//!                   A file's reading document has exactly one.
//!     reader        token   "text" for a text layer or plain text; the
//!                           recognizer's own token otherwise
//!     generation    int     raised when a re-read is warranted (D8)
//!     status        token   read | empty | partial | failed
//!     reason        token   with failed: password | damaged
//!                           with partial: page-cap (absent while still reading)
//!     source        token   text-layer | recognized | mixed | plain
//!     confidence    int     0–100; absent for text
//!     pages_done    int     with partial: how far the reading has got
//!     pages/<gen>   a mergeable map for one generation: "1", "2", … → text.
//!                   Lines separated by "\n", in reading order.
//! ```
//!
//! `pages/<gen>` is a key of the entry map, spelled with the generation in it
//! (`pages/1`), beside `reader` and `status` — not a `pages` map holding one
//! map per generation. [`pages_key`] is the one place it is spelled.
//!
//! ## The three rules that make two devices converge
//!
//! **Every nested map is a mergeable map** — `ensure_mergeable_map`, never
//! `get_or_create_container`. Two devices that each create the entry for one
//! blob before they sync then hold the *same* container and their writes merge;
//! with an ordinary child container one device's whole entry would lose to the
//! other's (`koru_core::profile` records the same trap).
//!
//! **Pages are keyed by number in a map**, so the same page written twice, by
//! one device or two, is one value and never two list items.
//!
//! **Each generation has its own pages map**, and a reader of the document
//! takes only the map named by the entry's `generation`. Two devices at
//! different generations therefore never leave a mixture of old and new pages.
//! The device that raises the generation empties the older maps; anything a
//! straggler writes there afterwards is inert, because nothing reads it.
//!
//! ## What is last-writer-wins, and why that is accepted
//!
//! The scalars (`status`, `generation`, …) are ordinary map values, so two
//! concurrent writes resolve by Loro's ordering, the same on every replica.
//! Within one generation a slower device's `partial` can briefly sit over a
//! faster one's `read`; the coordinator sees an unfinished reading and finishes
//! it. Two devices that raise *different* generations concurrently can leave
//! the lower number standing for a moment; the higher build then sees an entry
//! below its generation, reads it again and writes its number with a later
//! operation. In neither case does a reader ever see pages from two
//! generations, because it reads one map.
//!
//! ## No clock, no device, no positions
//!
//! `read_at` and the reading device are absent on purpose: Loro carries the
//! peer, and nothing displays a time. Word positions are absent until a later
//! generation adds them.

use std::collections::BTreeMap;

mod corrections;
pub use corrections::{CorrectionRevision, Corrections};

use loro::{ExportMode, LoroDoc, LoroMap, LoroValue, ValueOrContainer};

use crate::reading::{PageText, ReaderId, Reading, ReadingStatus, Reason, Source};

/// The document's only root container.
pub const ROOT: &str = "readings";

/// The keys of one entry, spelled once.
pub mod key {
    pub const READER: &str = "reader";
    pub const GENERATION: &str = "generation";
    pub const STATUS: &str = "status";
    pub const REASON: &str = "reason";
    pub const SOURCE: &str = "source";
    pub const CONFIDENCE: &str = "confidence";
    pub const PAGES_DONE: &str = "pages_done";
    /// Every pages map's key starts with this; [`super::pages_key`] adds the
    /// generation.
    pub const PAGES_PREFIX: &str = "pages/";
}

/// The key of one generation's pages map: `pages/<generation>`.
pub fn pages_key(generation: u32) -> String {
    format!("{}{generation}", key::PAGES_PREFIX)
}

/// What went wrong writing or reading a reading document.
///
/// None of these carries a word of the document or a file's name; they are
/// safe to log.
#[derive(Debug)]
pub enum Error {
    /// Loro refused the operation or the bytes.
    Loro(String),
    /// A blob key that is not `<algorithm>:<lowercase hex>`. Refused before it
    /// can become a key in a synced document.
    InvalidHash,
    /// Pages or an outcome were written for a blob nobody has begun reading.
    /// Call [`ReadingDoc::begin`] first.
    NotBegun,
    /// A page number of 0; pages count from 1.
    InvalidPage,
    /// A confidence above 100.
    InvalidConfidence,
    /// A status with a reason it does not admit — `failed` with no reason,
    /// `read` with `page-cap`, … ([`ReadingStatus::admits`]) — or a whole
    /// reading written with no reader or no status.
    Inconsistent,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Loro(why) => write!(f, "loro: {why}"),
            Error::InvalidHash => f.write_str("a blob key is not <algorithm>:<lowercase hex>"),
            Error::NotBegun => f.write_str("nothing has begun reading this blob"),
            Error::InvalidPage => f.write_str("pages count from 1"),
            Error::InvalidConfidence => f.write_str("confidence is 0 to 100"),
            Error::Inconsistent => f.write_str("a status with a reason it does not admit"),
        }
    }
}

impl std::error::Error for Error {}

impl From<loro::LoroError> for Error {
    fn from(e: loro::LoroError) -> Error {
        Error::Loro(e.to_string())
    }
}

impl From<loro::LoroEncodeError> for Error {
    fn from(e: loro::LoroEncodeError) -> Error {
        Error::Loro(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// What a write did.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Write {
    Applied,
    /// Nothing was written: the entry is already at `generation`, which is
    /// above the one this write was for. A newer build, or a job that started
    /// before the generation was raised, is behind; its words are not wanted.
    Superseded {
        generation: u32,
    },
}

/// How a reading ended, for [`ReadingDoc::finish`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Outcome {
    pub status: ReadingStatus,
    pub reason: Option<Reason>,
    pub source: Option<Source>,
    pub confidence: Option<u8>,
    pub pages_done: Option<u32>,
}

/// Whether `hash` is the string form of a blob hash: `<algorithm>:<digest>`,
/// the algorithm an ASCII alphanumeric token and the digest lowercase hex,
/// both non-empty.
///
/// The same acceptance as `koru_store_core::model::BlobHash::parse` after it
/// lowercases — a test holds the two together — so every hash the store mints
/// is a key here, and nothing else is.
pub fn is_blob_hash(hash: &str) -> bool {
    let Some((algorithm, digest)) = hash.split_once(':') else {
        return false;
    };
    !algorithm.is_empty()
        && algorithm.chars().all(|c| c.is_ascii_alphanumeric())
        && !digest.is_empty()
        && digest
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// **A reading document**: an owned `LoroDoc` and the rules for writing it.
///
/// The Store daemon forks or loads the document's bytes under its own lock,
/// wraps them here, writes, and hands back the delta
/// ([`ReadingDoc::export_updates_since`]); nothing here touches a disk or a
/// store. Every write method commits before it returns, so a write is one
/// Loro change and never half of one.
pub struct ReadingDoc {
    doc: LoroDoc,
}

impl Default for ReadingDoc {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadingDoc {
    /// An empty reading document — what `Store::ensure_reading` registers.
    pub fn new() -> ReadingDoc {
        ReadingDoc {
            doc: LoroDoc::new(),
        }
    }

    /// Wrap a document the caller already holds (a fork, a load).
    pub fn from_doc(doc: LoroDoc) -> ReadingDoc {
        ReadingDoc { doc }
    }

    /// A document from a snapshot or a run of updates.
    pub fn from_bytes(bytes: &[u8]) -> Result<ReadingDoc> {
        let doc = LoroDoc::new();
        doc.import(bytes)?;
        Ok(ReadingDoc { doc })
    }

    pub fn doc(&self) -> &LoroDoc {
        &self.doc
    }

    pub fn into_doc(self) -> LoroDoc {
        self.doc
    }

    /// Merge another replica's changes.
    pub fn import(&self, bytes: &[u8]) -> Result<()> {
        self.doc.import(bytes)?;
        Ok(())
    }

    pub fn export_snapshot(&self) -> Result<Vec<u8>> {
        Ok(self.doc.export(ExportMode::snapshot())?)
    }

    pub fn export_updates_since(&self, from: &loro::VersionVector) -> Result<Vec<u8>> {
        Ok(self.doc.export(ExportMode::updates(from))?)
    }

    pub fn version(&self) -> loro::VersionVector {
        self.doc.oplog_vv()
    }

    fn root(&self) -> LoroMap {
        self.doc.get_map(ROOT)
    }

    // ------------------------------------------------------------ reading

    /// Every blob this document holds an entry for, sorted.
    pub fn hashes(&self) -> Vec<String> {
        let root = self.root();
        let mut out: Vec<String> = root
            .keys()
            .map(|k| k.to_string())
            .filter(|k| matches!(root.get(k), Some(ValueOrContainer::Container(_))))
            .collect();
        out.sort();
        out
    }

    /// The entry for `hash`, with the pages of its own generation only.
    ///
    /// `None` when there is no entry, or it has no generation — nothing a
    /// reader can take pages from.
    pub fn reading(&self, hash: &str) -> Option<Reading> {
        let entry = self.entry(hash)?;
        let generation = get_u32(&entry, key::GENERATION)?;
        let mut pages = BTreeMap::new();
        if let Some(map) = child_map(&entry, &pages_key(generation)) {
            for k in map.keys() {
                let Ok(page) = k.parse::<u32>() else {
                    continue;
                };
                if page == 0 {
                    continue;
                }
                if let Some(text) = map.get(&k).and_then(as_string) {
                    pages.insert(page, PageText::new(text));
                }
            }
        }
        Some(Reading {
            corrections: corrections::read(&entry),
            reader: get_string(&entry, key::READER).and_then(ReaderId::parse),
            generation,
            status: get_string(&entry, key::STATUS).and_then(|s| ReadingStatus::parse(&s)),
            reason: get_string(&entry, key::REASON).and_then(|s| Reason::parse(&s)),
            source: get_string(&entry, key::SOURCE).and_then(|s| Source::parse(&s)),
            confidence: get_u32(&entry, key::CONFIDENCE)
                .and_then(|c| u8::try_from(c).ok())
                .filter(|c| *c <= 100),
            pages_done: get_u32(&entry, key::PAGES_DONE),
            pages,
        })
    }

    /// Every entry, sorted by hash.
    pub fn readings(&self) -> Vec<(String, Reading)> {
        self.hashes()
            .into_iter()
            .filter_map(|h| self.reading(&h).map(|r| (h, r)))
            .collect()
    }

    /// The generation an entry is at, read raw — for a writer deciding whether
    /// it is behind, even when the rest of the entry is in a shape this build
    /// does not understand.
    pub fn generation(&self, hash: &str) -> Option<u32> {
        get_u32(&self.entry(hash)?, key::GENERATION)
    }

    fn entry(&self, hash: &str) -> Option<LoroMap> {
        child_map(&self.root(), hash)
    }

    // ------------------------------------------------------------ writing

    /// The entry map for `hash`, created mergeable if it is not there.
    fn ensure_entry(&self, hash: &str) -> Result<LoroMap> {
        if !is_blob_hash(hash) {
            return Err(Error::InvalidHash);
        }
        Ok(self.root().ensure_mergeable_map(hash)?)
    }

    /// The entry, checked to be at exactly `generation`.
    fn entry_at(&self, hash: &str, generation: u32) -> Result<std::result::Result<LoroMap, Write>> {
        if !is_blob_hash(hash) {
            return Err(Error::InvalidHash);
        }
        let Some(entry) = self.entry(hash) else {
            return Err(Error::NotBegun);
        };
        match get_u32(&entry, key::GENERATION) {
            None => Err(Error::NotBegun),
            Some(current) if current > generation => Ok(Err(Write::Superseded {
                generation: current,
            })),
            // Below: this writer is ahead of what it last saw, which means it
            // never called `begin` at its own generation.
            Some(current) if current < generation => Err(Error::NotBegun),
            Some(_) => Ok(Ok(entry)),
        }
    }

    /// **Begin reading `hash` at `generation`** — the first write of a job.
    ///
    /// - Above the entry's generation, or with no entry: the generation is
    ///   raised and every older pages map is emptied.
    /// - At it (*Read again*, or a resumed `partial`): the pages stay — the
    ///   same pages are about to be written again under the same keys — and
    ///   the status is reset.
    /// - Below it: nothing is written; [`Write::Superseded`].
    ///
    /// Either way that writes, `status` becomes `partial` with no reason,
    /// which is what "still being read" is.
    pub fn begin(&self, hash: &str, reader: &ReaderId, generation: u32) -> Result<Write> {
        let entry = self.ensure_entry(hash)?;
        let current = get_u32(&entry, key::GENERATION);
        if let Some(current) = current
            && current > generation
        {
            return Ok(Write::Superseded {
                generation: current,
            });
        }
        if current != Some(generation) {
            self.empty_pages_below(&entry, generation)?;
            entry.insert(key::GENERATION, i64::from(generation))?;
            entry.delete(key::PAGES_DONE)?;
            entry.delete(key::CONFIDENCE)?;
            entry.delete(key::SOURCE)?;
        }
        entry.insert(key::READER, reader.as_str())?;
        entry.insert(key::STATUS, ReadingStatus::Partial.as_str())?;
        entry.delete(key::REASON)?;
        // So a reader taking this generation's map finds one, even before the
        // first page — and so the map exists mergeable from the start.
        entry.ensure_mergeable_map(&pages_key(generation))?;
        self.doc.commit();
        Ok(Write::Applied)
    }

    /// Start over at this generation, replacing derived words and progress.
    /// Resuming uses `begin` instead. A newer generation is never cleared.
    pub fn begin_fresh(&self, hash: &str, reader: &ReaderId, generation: u32) -> Result<Write> {
        let written = self.begin(hash, reader, generation)?;
        if written != Write::Applied {
            return Ok(written);
        }
        let entry = self.ensure_entry(hash)?;
        entry
            .ensure_mergeable_map(&pages_key(generation))?
            .clear()?;
        entry.delete(key::PAGES_DONE)?;
        entry.delete(key::CONFIDENCE)?;
        entry.delete(key::SOURCE)?;
        self.doc.commit();
        Ok(Write::Applied)
    }

    /// Clear every `pages/<g>` with `g` below `generation`. Cleared rather
    /// than deleted: deleting a mergeable key only hides its child, and the
    /// next `ensure_mergeable_map` would bring the old pages back.
    fn empty_pages_below(&self, entry: &LoroMap, generation: u32) -> Result<()> {
        let older: Vec<String> = entry
            .keys()
            .map(|k| k.to_string())
            .filter(|k| {
                k.strip_prefix(key::PAGES_PREFIX)
                    .and_then(|g| g.parse::<u32>().ok())
                    .is_some_and(|g| g < generation)
            })
            .collect();
        for k in older {
            if let Some(map) = child_map(entry, &k) {
                map.clear()?;
            }
        }
        Ok(())
    }

    /// **Write pages** of `hash` at `generation`: `(page number, words)`.
    ///
    /// Several pages in one call are one Loro change. `pages_done`, when
    /// given, records how far the reading has got, so a restart resumes after
    /// it. An empty page is not written: the index emits nothing for one, and
    /// a key holding `""` would be a page that says nothing on every device.
    pub fn put_pages(
        &self,
        hash: &str,
        generation: u32,
        pages: &[(u32, &PageText)],
        pages_done: Option<u32>,
    ) -> Result<Write> {
        if pages.iter().any(|(n, _)| *n == 0) {
            return Err(Error::InvalidPage);
        }
        let entry = match self.entry_at(hash, generation)? {
            Ok(entry) => entry,
            Err(superseded) => return Ok(superseded),
        };
        let map = entry.ensure_mergeable_map(&pages_key(generation))?;
        for (page, text) in pages {
            if !text.is_empty() {
                map.insert(&page.to_string(), text.as_str())?;
            }
        }
        if let Some(done) = pages_done {
            entry.insert(key::PAGES_DONE, i64::from(done))?;
        }
        self.doc.commit();
        Ok(Write::Applied)
    }

    /// **Record how the reading of `hash` at `generation` ended.**
    ///
    /// Each field of `outcome` that is `None` removes its key, so an entry
    /// says exactly what the last finish said. Refuses a status with a reason
    /// it does not admit, and a confidence above 100.
    pub fn finish(&self, hash: &str, generation: u32, outcome: &Outcome) -> Result<Write> {
        if !outcome.status.admits(outcome.reason) {
            return Err(Error::Inconsistent);
        }
        if outcome.confidence.is_some_and(|c| c > 100) {
            return Err(Error::InvalidConfidence);
        }
        let entry = match self.entry_at(hash, generation)? {
            Ok(entry) => entry,
            Err(superseded) => return Ok(superseded),
        };
        write_outcome(&entry, outcome)?;
        self.doc.commit();
        Ok(Write::Applied)
    }

    /// **Write a whole reading for `hash` at once** — the coordinator copying
    /// words this store root already has for the same bytes, instead of
    /// reading them again (TECHNICAL §9). Begin, pages and finish, in one
    /// change.
    ///
    /// `reading.reader` and `reading.status` must be set; the reading is
    /// written at its own `generation`, under the same rules as
    /// [`ReadingDoc::begin`].
    pub fn write(&self, hash: &str, reading: &Reading) -> Result<Write> {
        let (Some(reader), Some(status)) = (&reading.reader, reading.status) else {
            return Err(Error::Inconsistent);
        };
        let outcome = Outcome {
            status,
            reason: reading.reason,
            source: reading.source,
            confidence: reading.confidence,
            pages_done: reading.pages_done,
        };
        if !outcome.status.admits(outcome.reason) {
            return Err(Error::Inconsistent);
        }
        if outcome.confidence.is_some_and(|c| c > 100) {
            return Err(Error::InvalidConfidence);
        }
        if reading.pages.contains_key(&0) {
            return Err(Error::InvalidPage);
        }
        let entry = self.ensure_entry(hash)?;
        let generation = reading.generation;
        if let Some(current) = get_u32(&entry, key::GENERATION)
            && current > generation
        {
            return Ok(Write::Superseded {
                generation: current,
            });
        }
        if get_u32(&entry, key::GENERATION) != Some(generation) {
            self.empty_pages_below(&entry, generation)?;
            entry.insert(key::GENERATION, i64::from(generation))?;
        }
        corrections::merge(&entry, &reading.corrections)?;
        entry.insert(key::READER, reader.as_str())?;
        let map = entry.ensure_mergeable_map(&pages_key(generation))?;
        for (page, text) in &reading.pages {
            if !text.is_empty() {
                map.insert(&page.to_string(), text.as_str())?;
            }
        }
        write_outcome(&entry, &outcome)?;
        self.doc.commit();
        Ok(Write::Applied)
    }
}

fn write_outcome(entry: &LoroMap, outcome: &Outcome) -> Result<()> {
    entry.insert(key::STATUS, outcome.status.as_str())?;
    put_or_delete(
        entry,
        key::REASON,
        outcome.reason.map(|r| r.as_str().into()),
    )?;
    put_or_delete(
        entry,
        key::SOURCE,
        outcome.source.map(|s| s.as_str().into()),
    )?;
    put_or_delete(
        entry,
        key::CONFIDENCE,
        outcome.confidence.map(|c| LoroValue::I64(i64::from(c))),
    )?;
    put_or_delete(
        entry,
        key::PAGES_DONE,
        outcome.pages_done.map(|d| LoroValue::I64(i64::from(d))),
    )?;
    Ok(())
}

fn put_or_delete(map: &LoroMap, key: &str, value: Option<LoroValue>) -> Result<()> {
    match value {
        Some(v) => map.insert(key, v)?,
        None => map.delete(key)?,
    }
    Ok(())
}

fn child_map(map: &LoroMap, key: &str) -> Option<LoroMap> {
    match map.get(key)? {
        ValueOrContainer::Container(c) => c.into_map().ok(),
        ValueOrContainer::Value(_) => None,
    }
}

fn as_string(v: ValueOrContainer) -> Option<String> {
    match v.as_value()? {
        LoroValue::String(s) => Some(s.to_string()),
        _ => None,
    }
}

fn get_string(map: &LoroMap, key: &str) -> Option<String> {
    map.get(key).and_then(as_string)
}

fn get_u32(map: &LoroMap, key: &str) -> Option<u32> {
    match map.get(key)?.as_value()? {
        LoroValue::I64(i) => u32::try_from(*i).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
