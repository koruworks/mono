// SPDX-License-Identifier: MPL-2.0

//! Pending revisions of a page document: immutable, self-contained, and found
//! only through a record written last (page-document plan §6.5).
//!
//! The [outbox](super::outbox) buffers a session's owed work as one mutable
//! file per replica, replaced in place and deleted unconditionally by its own
//! writer. That is safe for the writer's own use and is no basis for anybody
//! else to adopt the file: a cleaner that decides a path is finished can
//! delete a newer revision written to that same path a moment later. A page
//! document is written before it exists anywhere else, and whoever registers
//! it may be another process, so its pending work has to be adoptable by
//! anyone — which means **no file or directory name is ever reused, nothing
//! is shared between revisions, and deletion only ever targets a named
//! immutable object whose content the store already holds.**
//!
//! ```text
//! <store root>/outbox/<workspace>/<document>/
//!   <replica>.<rev>/                one directory per immutable revision
//!     updates.loro                  Loro updates since this replica's last consumed revision
//!     <hash>.blob ...               every image this revision needs, its own copy
//!   <replica>.<rev>.json            the record — written last; the only way a revision is found
//! ```
//!
//! ## The discipline, as step machines
//!
//! Writing: create the directory under a fresh `.part` name, write the updates
//! and every blob into it, rename it to its final name (never used before),
//! then write the record through a `.part` and a rename. Releasing: delete
//! the record first, then the files, then the directory. Each is a
//! [`WriteRevision`] or a [`ReleaseRevision`] that advances **one filesystem
//! operation per `step`**, over a [`RevisionFs`] rather than `std::fs`
//! directly — so the interleaving model in `tests.rs` can run one writer, two
//! cleaners and a store whose confirmation is delayed or lost, with any actor
//! dying at any step, and prove what the plan claims: every published
//! record's content is present or confirmed, no unconfirmed content is ever
//! deleted, repeated clean-up is a no-op. The disk runs the same machines to
//! completion.
//!
//! ## What a directory without a record is
//!
//! A revision exists only through its record. A directory under a final name
//! with no record is **incomplete**: its writer died between the rename and
//! the record, or a cleaner died between deleting the record and deleting the
//! directory. Nobody consumes it. An adopter may delete it only when its
//! writer is gone or has moved past it ([`WriterStatus`]) **and** the store
//! holds its content ([`Confirmation::covers_updates`]); otherwise it is
//! kept, named and counted, so the shutdown guard can say so. A record whose
//! directory or a listed blob is missing is **damaged** and surfaced the same
//! way, never consumed.
//!
//! Why one operation per step is a faithful model: a rename is atomic; a torn
//! `updates.loro` or blob can only exist inside a `.part` directory, which
//! nothing reads; a torn record can only exist under a `.part` name, which
//! discovery skips. So every state the disk can be left in is one the model
//! visits.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The updates file inside a revision directory.
pub const UPDATES: &str = "updates.loro";
/// The extension of a blob copy inside a revision directory: `<hash>.blob`.
pub const BLOB_EXT: &str = "blob";
/// The extension of a revision's record beside its directory.
pub const RECORD_EXT: &str = "json";
/// The suffix of anything not yet published — a directory being filled or a
/// record being written. Discovery skips it; only its owner touches it.
pub const PART_SUFFIX: &str = ".part";

/// The filesystem the step machines run over.
///
/// `std::fs` on the disk; an in-memory map in the model. Every method is one
/// operation, because a step is one operation — the model's whole claim rests
/// on crashing between any two.
pub trait RevisionFs {
    /// Create `dir` and any missing parent.
    fn create_dir(&mut self, dir: &Path) -> io::Result<()>;
    /// Write a whole file; the parent must exist.
    fn write_file(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    /// Rename a file or a directory, atomically.
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_file(&mut self, path: &Path) -> io::Result<()>;
    /// Remove an **empty** directory.
    fn remove_dir(&mut self, dir: &Path) -> io::Result<()>;
    fn read_file(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// The entries of a directory; a missing directory lists as empty, which
    /// is the ordinary case for a document nothing is pending on.
    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>>;
}

/// One directory entry, as [`RevisionFs::list`] answers it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// What a cleaner has to know before it may delete anything: whether the
/// store durably holds the content.
///
/// The release rule, verbatim from the plan: a revision goes **only when the
/// store's persisted version covers every operation in it and every blob it
/// holds is confirmed** in the store's object store. Never a timer, never
/// "no Recover seen locally".
pub trait Confirmation {
    /// Whether the store's persisted version contains `version`, a record's
    /// encoded version vector.
    fn covers(&self, version: &[u8]) -> bool;
    /// The same question for a directory that has no record to say its
    /// version — answered from the updates bytes themselves.
    fn covers_updates(&self, updates: &[u8]) -> bool;
    fn blob_confirmed(&self, hash: &str) -> bool;
}

/// Whether a record-less directory's writer could still be about to write
/// its record.
pub trait WriterStatus {
    /// The revision `replica` is currently writing, or `None` when that
    /// writer is gone. A directory older than the current revision was
    /// abandoned; the current one is still in flight.
    fn current_revision(&self, replica: u64) -> Option<u64>;
}

/// `<replica>.<rev>` — the name of a revision's directory, and the stem of
/// its record. `rev` increases per writing replica; a name is never reused.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RevisionName {
    pub replica: u64,
    pub rev: u64,
}

impl RevisionName {
    pub fn dir_name(&self) -> String {
        format!("{:016x}.{}", self.replica, self.rev)
    }

    pub fn record_name(&self) -> String {
        format!("{}.{RECORD_EXT}", self.dir_name())
    }

    /// Parse a directory name, or a record name with its extension.
    pub fn parse(name: &str) -> Option<Self> {
        let stem = name.strip_suffix(&format!(".{RECORD_EXT}")).unwrap_or(name);
        let (replica, rev) = stem.split_once('.')?;
        if replica.len() != 16 {
            return None;
        }
        Some(Self {
            replica: u64::from_str_radix(replica, 16).ok()?,
            rev: rev.parse().ok()?,
        })
    }
}

/// The record beside a revision's directory — written last, and the only
/// way the revision is found.
///
/// It carries its own page key, so start-up discovery finds the revision
/// whatever the page currently selects, for any generation, a deleted list
/// or a workspace that is not active. Ids are their storage strings, so the
/// file reads the same on every build.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Record {
    pub workspace: String,
    pub document: String,
    /// `list:<id>` or `bucket:<token>` (`koru_core::ids::PageKey`).
    pub page: String,
    pub generation: u64,
    /// The registry state the revision was written in, as a token.
    pub state: String,
    /// Whether this replica has ever seen the document listed — a pending
    /// first creation that never has may seed; one that has never does.
    pub seen_listed: bool,
    /// The hash of every blob copy in the directory.
    pub blobs: Vec<String>,
    /// The document's encoded version after this revision, which is what the
    /// release rule compares with the store's persisted version.
    pub version: Vec<u8>,
}

impl Record {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("a record is plain data")
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

// ---------------------------------------------------------------- writing

/// Writing one revision, one operation per step.
///
/// The directory is filled under a `.part` name nobody else will ever use,
/// renamed to its final name once whole, and only then named by a record.
/// Dying at any step leaves either nothing anyone reads (a `.part`), or a
/// complete directory that discovery reports as incomplete and keeps.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct WriteRevision {
    name: RevisionName,
    part: PathBuf,
    dir: PathBuf,
    record_part: PathBuf,
    record: PathBuf,
    updates: Vec<u8>,
    blobs: Vec<(String, Vec<u8>)>,
    record_bytes: Vec<u8>,
    stage: WriteStage,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
enum WriteStage {
    CreatePart,
    WriteUpdates,
    WriteBlob(usize),
    RenameDir,
    WriteRecordPart,
    RenameRecord,
    Done,
}

impl WriteRevision {
    /// `part_token` names the directory while it is being filled —
    /// `.<token>.part` — and must be fresh per attempt; the disk uses a random
    /// one. The blobs are `(hash, bytes)`; the record must list exactly their
    /// hashes.
    pub fn new(
        document_dir: &Path,
        name: RevisionName,
        part_token: &str,
        updates: Vec<u8>,
        blobs: Vec<(String, Vec<u8>)>,
        record: &Record,
    ) -> Self {
        debug_assert_eq!(
            record.blobs,
            blobs.iter().map(|(h, _)| h.clone()).collect::<Vec<_>>(),
            "the record lists exactly the blobs the directory holds"
        );
        let record_name = name.record_name();
        Self {
            name,
            part: document_dir.join(format!(".{part_token}{PART_SUFFIX}")),
            dir: document_dir.join(name.dir_name()),
            record_part: document_dir.join(format!("{record_name}{PART_SUFFIX}")),
            record: document_dir.join(record_name),
            updates,
            blobs,
            record_bytes: record.encode(),
            stage: WriteStage::CreatePart,
        }
    }

    pub fn name(&self) -> RevisionName {
        self.name
    }

    pub fn is_done(&self) -> bool {
        self.stage == WriteStage::Done
    }

    /// Whether the updates have reached a directory — the point from which
    /// dying would lose something if the directory were ever deleted.
    pub fn updates_written(&self) -> bool {
        !matches!(
            self.stage,
            WriteStage::CreatePart | WriteStage::WriteUpdates
        )
    }

    /// Whether blob `index` has been written to the directory.
    pub fn blob_written(&self, index: usize) -> bool {
        match self.stage {
            WriteStage::CreatePart | WriteStage::WriteUpdates => false,
            WriteStage::WriteBlob(next) => index < next,
            _ => true,
        }
    }

    /// One filesystem operation. Returns whether the revision is published.
    pub fn step(&mut self, fs: &mut impl RevisionFs) -> io::Result<bool> {
        self.stage = match self.stage {
            WriteStage::CreatePart => {
                fs.create_dir(&self.part)?;
                WriteStage::WriteUpdates
            }
            WriteStage::WriteUpdates => {
                fs.write_file(&self.part.join(UPDATES), &self.updates)?;
                WriteStage::WriteBlob(0)
            }
            WriteStage::WriteBlob(i) if i < self.blobs.len() => {
                let (hash, bytes) = &self.blobs[i];
                fs.write_file(&self.part.join(blob_file(hash)), bytes)?;
                WriteStage::WriteBlob(i + 1)
            }
            WriteStage::WriteBlob(_) => {
                fs.rename(&self.part, &self.dir)?;
                WriteStage::RenameDir
            }
            WriteStage::RenameDir => {
                fs.write_file(&self.record_part, &self.record_bytes)?;
                WriteStage::WriteRecordPart
            }
            WriteStage::WriteRecordPart => {
                fs.rename(&self.record_part, &self.record)?;
                WriteStage::RenameRecord
            }
            WriteStage::RenameRecord | WriteStage::Done => WriteStage::Done,
        };
        Ok(self.stage == WriteStage::Done)
    }

    pub fn run(&mut self, fs: &mut impl RevisionFs) -> io::Result<()> {
        while !self.step(fs)? {}
        Ok(())
    }
}

fn blob_file(hash: &str) -> String {
    format!("{hash}.{BLOB_EXT}")
}

// -------------------------------------------------------------- discovery

/// A revision with its record and every file the record lists present.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Complete {
    pub name: RevisionName,
    pub record: Record,
    pub record_path: PathBuf,
    pub dir: PathBuf,
}

/// A directory under a final name with no record: never consumed, deleted
/// only by an adopter that has checked [`WriterStatus`] and
/// [`Confirmation::covers_updates`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Incomplete {
    pub name: RevisionName,
    pub dir: PathBuf,
}

/// A record that cannot be consumed: undecodable, or its directory or a
/// listed blob is missing. Surfaced, never deleted here.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Damaged {
    pub name: Option<RevisionName>,
    pub record_path: PathBuf,
    pub why: String,
}

/// Everything under one document's directory, classified.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Discovered {
    pub complete: Vec<Complete>,
    pub incomplete: Vec<Incomplete>,
    pub damaged: Vec<Damaged>,
    /// `.part` names — a writer's work in progress, or what a dead writer
    /// left. Only its owner cleans these.
    pub parts: Vec<PathBuf>,
}

impl Discovered {
    /// Whether anything here is owed: a complete revision the store may not
    /// hold yet, an incomplete directory, or a damaged record. What
    /// `Workspace::unconfirmed()` counts.
    pub fn is_empty(&self) -> bool {
        self.complete.is_empty() && self.incomplete.is_empty() && self.damaged.is_empty()
    }
}

/// Classify one document's directory. A record is the only way a revision
/// is found; a directory is only ever looked at through one, or reported as
/// incomplete.
pub fn discover(fs: &impl RevisionFs, document_dir: &Path) -> io::Result<Discovered> {
    let entries = fs.list(document_dir)?;
    let mut out = Discovered::default();
    let dirs: std::collections::BTreeSet<String> = entries
        .iter()
        .filter(|e| e.is_dir && !e.name.ends_with(PART_SUFFIX))
        .map(|e| e.name.clone())
        .collect();
    let mut named: std::collections::BTreeSet<RevisionName> = Default::default();

    for entry in &entries {
        if entry.name.ends_with(PART_SUFFIX) {
            out.parts.push(document_dir.join(&entry.name));
            continue;
        }
        if entry.is_dir || !entry.name.ends_with(&format!(".{RECORD_EXT}")) {
            continue;
        }
        let record_path = document_dir.join(&entry.name);
        let name = RevisionName::parse(&entry.name);
        let damaged = |why: String| Damaged {
            name,
            record_path: record_path.clone(),
            why,
        };
        let Some(name) = name else {
            out.damaged.push(damaged("not a revision name".into()));
            continue;
        };
        // Named by a record, damaged or not: never reported as incomplete,
        // so no adopter deletes a directory a record still points at.
        named.insert(name);
        let Some(record) = fs
            .read_file(&record_path)
            .ok()
            .and_then(|b| Record::decode(&b))
        else {
            out.damaged
                .push(damaged("the record will not decode".into()));
            continue;
        };
        let dir_name = name.dir_name();
        if !dirs.contains(&dir_name) {
            out.damaged.push(damaged("its directory is missing".into()));
            continue;
        }
        let dir = document_dir.join(&dir_name);
        let files: std::collections::BTreeSet<String> = fs
            .list(&dir)?
            .into_iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.name)
            .collect();
        if !files.contains(UPDATES) {
            out.damaged.push(damaged(format!("{UPDATES} is missing")));
            continue;
        }
        if let Some(missing) = record
            .blobs
            .iter()
            .find(|hash| !files.contains(&blob_file(hash)))
        {
            out.damaged
                .push(damaged(format!("blob {missing} is missing")));
            continue;
        }
        out.complete.push(Complete {
            name,
            record,
            record_path,
            dir,
        });
    }

    for dir_name in dirs {
        let Some(name) = RevisionName::parse(&dir_name) else {
            continue;
        };
        if !named.contains(&name) {
            out.incomplete.push(Incomplete {
                name,
                dir: document_dir.join(&dir_name),
            });
        }
    }
    out.complete.sort_by_key(|c| c.name);
    out.incomplete.sort_by_key(|i| i.name);
    Ok(out)
}

// ---------------------------------------------------------------- release

/// The release rule for a complete revision: the store's persisted version
/// covers it and every blob it holds is confirmed.
pub fn may_release(revision: &Complete, store: &impl Confirmation) -> bool {
    store.covers(&revision.record.version)
        && revision
            .record
            .blobs
            .iter()
            .all(|hash| store.blob_confirmed(hash))
}

/// Whether an adopter may delete a record-less directory: its writer is gone
/// or has moved past it, **and** the store holds its content — the updates
/// by their version, and every blob copy in the directory by its hash.
/// Otherwise it is kept.
pub fn may_adopt(
    incomplete: &Incomplete,
    fs: &impl RevisionFs,
    writers: &impl WriterStatus,
    store: &impl Confirmation,
) -> io::Result<bool> {
    let abandoned = match writers.current_revision(incomplete.name.replica) {
        None => true,
        Some(current) => incomplete.name.rev < current,
    };
    if !abandoned {
        return Ok(false);
    }
    let blob_suffix = format!(".{BLOB_EXT}");
    for entry in fs.list(&incomplete.dir)? {
        if let Some(hash) = entry.name.strip_suffix(&blob_suffix)
            && !store.blob_confirmed(hash)
        {
            return Ok(false);
        }
    }
    let updates = match fs.read_file(&incomplete.dir.join(UPDATES)) {
        Ok(bytes) => bytes,
        // No updates file: nothing in it the store could be asked about, and
        // nothing in it anyone could lose. Still only deleted once abandoned.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e),
    };
    Ok(store.covers_updates(&updates))
}

/// Deleting one revision whose content the store holds, one operation per
/// step: the record first — so nobody finds the revision again — then every
/// file, then the directory. Dying in the middle leaves a directory without
/// a record whose content is already confirmed, which [`may_adopt`] lets the
/// next cleaner finish.
///
/// Built from either a [`Complete`] revision (after [`may_release`]) or an
/// [`Incomplete`] directory (after [`may_adopt`]); a missing record or file
/// is not an error, because two cleaners may be releasing the same revision
/// and deletion of a named immutable object is idempotent.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ReleaseRevision {
    record: Option<PathBuf>,
    dir: PathBuf,
    files: Vec<PathBuf>,
    stage: ReleaseStage,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
enum ReleaseStage {
    RemoveRecord,
    RemoveFile(usize),
    RemoveDir,
    Done,
}

impl ReleaseRevision {
    pub fn of_complete(fs: &impl RevisionFs, revision: &Complete) -> io::Result<Self> {
        Self::new(fs, Some(revision.record_path.clone()), revision.dir.clone())
    }

    pub fn of_incomplete(fs: &impl RevisionFs, incomplete: &Incomplete) -> io::Result<Self> {
        Self::new(fs, None, incomplete.dir.clone())
    }

    fn new(fs: &impl RevisionFs, record: Option<PathBuf>, dir: PathBuf) -> io::Result<Self> {
        let files = fs
            .list(&dir)?
            .into_iter()
            .filter(|e| !e.is_dir)
            .map(|e| dir.join(e.name))
            .collect();
        Ok(Self {
            record,
            dir,
            files,
            stage: ReleaseStage::RemoveRecord,
        })
    }

    pub fn is_done(&self) -> bool {
        self.stage == ReleaseStage::Done
    }

    /// One filesystem operation. Returns whether the revision is gone.
    pub fn step(&mut self, fs: &mut impl RevisionFs) -> io::Result<bool> {
        self.stage = match self.stage {
            ReleaseStage::RemoveRecord => {
                if let Some(record) = &self.record {
                    ignore_missing(fs.remove_file(record))?;
                }
                ReleaseStage::RemoveFile(0)
            }
            ReleaseStage::RemoveFile(i) if i < self.files.len() => {
                ignore_missing(fs.remove_file(&self.files[i]))?;
                ReleaseStage::RemoveFile(i + 1)
            }
            ReleaseStage::RemoveFile(_) => {
                ignore_missing(fs.remove_dir(&self.dir))?;
                ReleaseStage::RemoveDir
            }
            ReleaseStage::RemoveDir | ReleaseStage::Done => ReleaseStage::Done,
        };
        Ok(self.stage == ReleaseStage::Done)
    }

    pub fn run(&mut self, fs: &mut impl RevisionFs) -> io::Result<()> {
        while !self.step(fs)? {}
        Ok(())
    }
}

fn ignore_missing(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests;
