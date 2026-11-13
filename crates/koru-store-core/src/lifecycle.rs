// SPDX-License-Identifier: MPL-2.0

//! A document the store is holding open, and the rules for writing it down.
//!
//! [`LiveDocument`] is a `LoroDoc` the store owns, plus the identity it is
//! filed under. **It knows nothing about what is inside it** — a manifest and a
//! user's document are the same thing here, which is what lets one persistence
//! path serve both.
//!
//! ## Group commit
//!
//! `synchronous = FULL` means one fsync per transaction. One transaction per
//! keystroke would put a disk sync in the typing path, which is unusable; the
//! answer is **not** to weaken the pragma but to widen the transaction.
//! [`CommitWindow`] collects updates that arrive close together and hands them
//! over as one [`UpdateBatch`]:
//!
//! ```text
//! arrive   u u  u    u                u
//!          └──────┬──────┘            └──┬──┘
//!                 │ one commit, one fsync   │
//!            ack all four              ack the fifth
//! ```
//!
//! The window changes **how often the disk is touched**. It never changes what
//! an acknowledgement means: nothing in a window is acknowledged until the
//! commit containing it has returned. A client that sends one update and waits
//! pays the window's latency once; a client that types pays it once per window
//! rather than once per key.
//!
//! ## Checkpoints
//!
//! [`CheckpointPolicy`] decides when a snapshot is worth taking. Both limits
//! exist because either alone has a bad case: counting updates alone lets a few
//! enormous ones sit unsnapshotted, and counting bytes alone lets thousands of
//! tiny ones pile up into a long replay.

use loro::{ExportMode, LoroDoc};

use crate::errors::{Code, Error, Result};
use crate::ids::DocumentId;
use crate::model::Timestamp;
use crate::ports::{Checkpoint, UpdateBatch, UpdateOrigin};

mod history;
pub use history::validate_history_evidence;

/// A `LoroDoc` the store is holding open.
///
/// Opaque by design: the store validates that an update applies and records
/// where the document has got to, and never looks at what changed.
pub struct LiveDocument {
    id: DocumentId,
    doc: LoroDoc,
}

/// Identity and position only — **never content.**
///
/// A live document ends up in assertion messages, log lines and the debug
/// routes, and every one of those is a place someone's writing must not appear.
impl std::fmt::Debug for LiveDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveDocument")
            .field("id", &self.id)
            .field("peer", &self.doc.peer_id())
            .finish_non_exhaustive()
    }
}

impl LiveDocument {
    /// An empty document. Used when nothing has ever been persisted for this
    /// id — the very first update becomes its whole history.
    pub fn empty(id: DocumentId) -> Self {
        Self {
            id,
            doc: LoroDoc::new(),
        }
    }

    /// Load from a checkpoint's snapshot bytes.
    pub fn from_snapshot(id: DocumentId, snapshot: &[u8]) -> Result<Self> {
        let doc = LoroDoc::new();
        doc.import(snapshot)
            .map_err(|e| Error::new(Code::CorruptState, format!("snapshot: {e}")))?;
        Ok(Self { id, doc })
    }

    pub fn id(&self) -> DocumentId {
        self.id
    }

    pub fn doc(&self) -> &LoroDoc {
        &self.doc
    }

    /// Pin this replica's Loro `PeerID`.
    ///
    /// The store's durable replica keeps one for the life of the install.
    /// Without it a `LoroDoc`'s peer is random per construction — **including
    /// every reload from a snapshot** — so a store restarted a hundred times
    /// would carry a hundred peers in every version vector it encodes or
    /// compares. The value names a replica, never a person and never a machine.
    pub fn set_peer_id(&self, peer: u64) -> Result<()> {
        self.doc
            .set_peer_id(peer)
            .map_err(|e| Error::new(Code::StorageFailed, format!("set_peer_id: {e}")))
    }

    pub fn peer_id(&self) -> u64 {
        self.doc.peer_id()
    }

    // ------------------------------------------------------------- transport

    pub fn export_snapshot(&self) -> Result<Vec<u8>> {
        self.doc
            .export(ExportMode::snapshot())
            .map_err(|e| Error::new(Code::StorageFailed, format!("export snapshot: {e}")))
    }

    pub fn export_all_updates(&self) -> Result<Vec<u8>> {
        self.doc
            .export(ExportMode::all_updates())
            .map_err(|e| Error::new(Code::StorageFailed, format!("export updates: {e}")))
    }

    pub fn export_updates_since(&self, from: &loro::VersionVector) -> Result<Vec<u8>> {
        self.doc
            .export(ExportMode::updates(from))
            .map_err(|e| Error::new(Code::StorageFailed, format!("export updates: {e}")))
    }

    pub fn version(&self) -> loro::VersionVector {
        self.doc.oplog_vv()
    }

    pub fn frontiers(&self) -> loro::Frontiers {
        self.doc.oplog_frontiers()
    }

    /// The encoded forms that get written into `loro_documents`.
    pub fn encoded_version(&self) -> Vec<u8> {
        self.version().encode()
    }

    pub fn encoded_frontiers(&self) -> Vec<u8> {
        self.frontiers().encode()
    }

    /// Export immutable state at an exact frontier already present in this
    /// replica's history. The live document is never checked out or mutated.
    pub fn snapshot_at(&self, encoded: Option<&[u8]>) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        let fork = match encoded {
            None => self.doc.fork(),
            Some(bytes) => {
                let frontiers = loro::Frontiers::decode(bytes).map_err(|e| {
                    Error::new(Code::InvalidRequest, format!("decode frontiers: {e}"))
                })?;
                self.doc.fork_at(&frontiers).map_err(|e| {
                    Error::new(Code::VersionUnavailable, format!("fork at frontiers: {e}"))
                })?
            }
        };
        let snapshot = fork.export(ExportMode::snapshot()).map_err(|e| {
            Error::new(Code::StorageFailed, format!("export history snapshot: {e}"))
        })?;
        Ok((
            snapshot,
            fork.oplog_vv().encode(),
            fork.oplog_frontiers().encode(),
        ))
    }

    /// Apply one update.
    ///
    /// Malformed encoding is refused before journaling. Ordinary device imports
    /// retain Loro's pending-ancestor behavior; Cloud imports additionally use
    /// `import_cloud` to reject incomplete causal updates without live mutation.
    pub fn import(&mut self, bytes: &[u8]) -> Result<()> {
        self.doc
            .import(bytes)
            .map_err(|e| Error::new(Code::CrdtRejected, format!("import: {e}")))?;
        Ok(())
    }

    /// Cloud admission promises a complete causal update. Stage on Loro's fork
    /// so a partially applied + pending import cannot change or contaminate live
    /// state on refusal. Ordinary editor/device imports keep their existing path.
    pub(crate) fn import_cloud(&mut self, bytes: &[u8]) -> Result<()> {
        validate_cloud_import(&self.doc, bytes)?;
        // The owner still holds the exclusive Store boundary. Do not replace
        // the live document with its fork: older device imports may own pending
        // operations which a fork is not required to retain.
        self.import(bytes)
    }

    /// Whether this replica already has everything in a frame, read from the
    /// frame's header alone — without decoding its body.
    pub fn frame_is_known(&self, frame: &[u8]) -> bool {
        match LoroDoc::decode_import_blob_meta(frame, false) {
            Ok(meta) => self.version().includes_vv(&meta.partial_end_vv),
            Err(_) => false,
        }
    }

    /// A checkpoint of the document as it stands, covering everything through
    /// `seq`.
    pub fn checkpoint(&self, seq: u64, at: Timestamp) -> Result<Checkpoint> {
        let snapshot = self.export_snapshot()?;
        Ok(Checkpoint {
            seq,
            checksum: checksum(&snapshot),
            snapshot,
            version_vector: self.encoded_version(),
            frontiers: self.encoded_frontiers(),
            created_at: at,
        })
    }
}

/// The checksum a checkpoint is verified against — BLAKE3, hex.
///
/// **One hash function in the store, computed in one place.** An earlier draft
/// had the persistence layer inject this so that core need not link `blake3`;
/// that indirection could be left uninstalled, and the failure mode was empty
/// checksums that every verifier accepts. A silent downgrade of the thing that
/// detects corruption is worse than a dependency, and `blake3` is a small
/// pure-Rust one.
///
/// It is also the same function that addresses blobs, so the store has one hash
/// rather than two, and it is fast enough that verifying on every load is not a
/// reason to skip verifying.
pub fn checksum(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub(crate) fn validate_cloud_import(doc: &LoroDoc, bytes: &[u8]) -> Result<()> {
    let staged = doc.fork();
    staged
        .set_peer_id(doc.peer_id())
        .map_err(|_| Error::new(Code::CrdtRejected, "Cloud staging failed"))?;
    let imported = staged
        .import(bytes)
        .map_err(|_| Error::new(Code::CrdtRejected, "Cloud update rejected"))?;
    if imported.pending.is_some() {
        return Err(Error::new(
            Code::CrdtRejected,
            "Cloud update has missing causal ancestors",
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------ group commit

/// Ephemeral resources that must live as long as queued update bytes.
pub(crate) type UpdateRetention = Box<dyn Send + Sync>;

/// Keeps resources through post-commit processing as well as the write itself.
pub(crate) struct CommittedWindow {
    pub head: u64,
    pub batch: UpdateBatch,
    _retained: Vec<UpdateRetention>,
}

/// Updates collected for one transaction.
///
/// Not a timer — the caller owns the clock, so a test can drive a window
/// deterministically and the daemon can drive it from its event loop. This only
/// answers *what has accumulated* and *is it time*.
pub struct CommitWindow {
    document: DocumentId,
    updates: Vec<Vec<u8>>,
    origins: Vec<UpdateOrigin>,
    retained: Vec<UpdateRetention>,
    opened_at: Option<Timestamp>,
    /// How long a window stays open. 5 ms by default: long enough that a burst
    /// of typing shares one fsync, short enough that a lone edit is not
    /// noticeably delayed.
    pub window: i64,
    /// A hard cap, so a flood commits rather than growing without bound.
    pub max_updates: usize,
}

impl CommitWindow {
    pub const DEFAULT_WINDOW_MS: i64 = 5;
    pub const DEFAULT_MAX_UPDATES: usize = 512;

    pub fn new(document: DocumentId) -> Self {
        Self {
            document,
            updates: Vec::new(),
            origins: Vec::new(),
            retained: Vec::new(),
            opened_at: None,
            window: Self::DEFAULT_WINDOW_MS,
            max_updates: Self::DEFAULT_MAX_UPDATES,
        }
    }

    pub fn push(&mut self, bytes: Vec<u8>, at: Timestamp, origin: UpdateOrigin) {
        self.push_retained(bytes, at, origin, None);
    }

    pub(crate) fn push_retained(
        &mut self,
        bytes: Vec<u8>,
        at: Timestamp,
        origin: UpdateOrigin,
        retention: Option<UpdateRetention>,
    ) {
        if self.updates.is_empty() {
            self.opened_at = Some(at);
        }
        self.updates.push(bytes);
        self.origins.push(origin);
        if let Some(retention) = retention {
            self.retained.push(retention);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.updates.is_empty()
    }

    pub fn len(&self) -> usize {
        self.updates.len()
    }

    /// Whether this window should be committed now.
    pub fn due(&self, now: Timestamp) -> bool {
        match self.opened_at {
            None => false,
            Some(opened) => {
                self.updates.len() >= self.max_updates
                    || now.millis().saturating_sub(opened.millis()) >= self.window
            }
        }
    }

    /// When this window will be due, for an event loop deciding how long to
    /// sleep. `None` when there is nothing waiting.
    pub fn due_at(&self) -> Option<Timestamp> {
        self.opened_at
            .map(|o| Timestamp::from_millis(o.millis() + self.window))
    }

    /// Commit the accumulated batch, retaining its original bytes, origins and
    /// due time on failure. A known-frame retry may skip re-import but must not
    /// forget the write that is still owed.
    ///
    /// The callback borrows the batch; it cannot consume our retry obligation.
    /// The returned batch is available for post-commit notifications only after
    /// the repository reports success.
    pub(crate) fn commit(
        &mut self,
        version_vector: Vec<u8>,
        frontiers: Vec<u8>,
        at: Timestamp,
        write: impl FnOnce(&UpdateBatch) -> Result<u64>,
    ) -> Result<Option<CommittedWindow>> {
        let opened_at = self.opened_at;
        let Some(batch) = self.take(version_vector, frontiers, at) else {
            return Ok(None);
        };
        match write(&batch) {
            Ok(head) => Ok(Some(CommittedWindow {
                head,
                batch,
                _retained: std::mem::take(&mut self.retained),
            })),
            Err(error) => {
                self.updates = batch.updates;
                self.origins = batch.origins;
                self.opened_at = opened_at;
                Err(error)
            }
        }
    }

    /// Take everything accumulated, leaving the window empty and closed.
    ///
    /// The position is passed in rather than read from a [`LiveDocument`],
    /// because the manifest is not one — it is a [`Manifest`], which holds its
    /// own `LoroDoc`. Taking the encoded position as an argument lets both go
    /// through one commit path instead of two that can drift.
    ///
    /// It must be the position **after** the updates have been imported, so
    /// what is written is what those updates actually produced rather than a
    /// prediction of it.
    ///
    /// [`Manifest`]: crate::manifest::Manifest
    pub fn take(
        &mut self,
        version_vector: Vec<u8>,
        frontiers: Vec<u8>,
        at: Timestamp,
    ) -> Option<UpdateBatch> {
        if self.updates.is_empty() {
            return None;
        }
        let batch = UpdateBatch {
            document: self.document,
            updates: std::mem::take(&mut self.updates),
            version_vector,
            frontiers,
            received_at: at,
            origins: std::mem::take(&mut self.origins),
        };
        self.opened_at = None;
        Some(batch)
    }
}

// ------------------------------------------------------------------ checkpoints

/// When a snapshot is worth taking.
#[derive(Copy, Clone, Debug)]
pub struct CheckpointPolicy {
    pub every_updates: u64,
    pub every_bytes: u64,
}

impl Default for CheckpointPolicy {
    fn default() -> Self {
        Self {
            every_updates: 200,
            every_bytes: 1 << 20,
        }
    }
}

impl CheckpointPolicy {
    /// Both limits, because either alone has a bad case: counting updates lets
    /// a few enormous ones sit unsnapshotted, and counting bytes lets thousands
    /// of tiny ones pile into a long replay.
    pub fn due(&self, updates_since: u64, bytes_since: u64) -> bool {
        updates_since >= self.every_updates || bytes_since >= self.every_bytes
    }
}

#[cfg(test)]
mod tests;

/// P1.0's measurement of the delegated Run document through this seam.
#[cfg(test)]
mod run_storage;
