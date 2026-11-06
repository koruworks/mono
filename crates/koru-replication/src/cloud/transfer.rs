// SPDX-License-Identifier: MPL-2.0

//! Cloud-only carrier. An accepted handshake is necessary, never sufficient:
//! composition must check current mode/authority at admission and durable handoff.
//! No listener, database, peer identity or alternative Loro engine lives here.

mod claim;
mod codec;
mod fragments;
pub mod metadata;
mod state;
pub use claim::{ClaimReferences, UploadContext};
pub use codec::{
    Control, Decision, Record, Start, Unit, decode_control, decode_record, encode_record,
    payload_length,
};
pub use fragments::Outgoing;
pub use state::{Admission, Completed, Scope, Stream, StreamStatus};

use super::Refusal;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub const MAX_PAYLOAD: usize = 16_777_216;
pub const MAX_CONTROL: usize = 4096;
pub const RESTRICTED_LIMIT: usize = 200_000;
pub const FRAGMENT_BYTES: usize = 200 * 1024;
pub const CONTROL_WAIT_MS: u64 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    /// The peer stopped reading or writing: an end of stream, a reset, or any
    /// other I/O failure on the carrier. It is deliberately NOT `Invalid`:
    /// nothing the peer sent was refused, so this must never send a reader
    /// looking for a malformed record. A device closes its stream when its
    /// pass is done, which is the ordinary way a session ends.
    Closed,
    Timeout,
    PolicyChanged,
    TooLarge,
    Unavailable,
    /// `sync_unit_exists` (cloud-sync-admission, the app connection
    /// amendment; DS-D44): an app connection uploaded a creation, a blob
    /// rooted at the empty document, for a unit Cloud already holds. The
    /// worker refuses it before `reserve`, so the unit's head did not move.
    /// Only a connection's session ever produces or receives it.
    UnitExists,
    /// `sync_document_claimed` (cloud-sync-admission, the app connection
    /// amendment; DS-D45): another holder has a live claim on the document,
    /// or this upload's claim reference is not the claim's. The worker
    /// refuses it before `reserve`, or rolls back its publish transaction, so
    /// the unit's head did not move. Only a connection's session ever
    /// produces or receives it.
    DocumentClaimed,
    Authority(Refusal),
}
impl Error {
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "sync_transfer_invalid",
            Self::Closed => "sync_peer_closed",
            Self::Timeout => "sync_transfer_timeout",
            Self::PolicyChanged => "sync_policy_changed",
            Self::TooLarge => "sync_update_too_large",
            Self::Unavailable => "sync_unavailable",
            Self::UnitExists => "sync_unit_exists",
            Self::DocumentClaimed => "sync_document_claimed",
            Self::Authority(e) => e.code(),
        }
    }
    pub fn frame(self) -> crate::protocol::PeerFrame {
        crate::protocol::PeerFrame::Error {
            code: self.code().into(),
        }
    }
}

/// The encoded empty version vector: what a unit the Cloud has never held
/// answers a join with, so the device sends everything it has. Callers outside
/// this crate ask for it here rather than spelling `loro` themselves — the
/// version's encoding belongs to the crate that decodes it.
pub fn empty_version() -> Vec<u8> {
    loro::VersionVector::default().encode()
}

pub fn budget_millis(length: usize) -> Result<u64, Error> {
    if !(1..=MAX_PAYLOAD).contains(&length) {
        return Err(Error::Invalid);
    }
    Ok((30 + length.div_ceil(32768) as u64) * 1000)
}

/// One shared connection/worker budget, not a fresh allowance per stream.
/// RAII reservations survive handoff while bytes still exist.
#[derive(Clone)]
pub struct MemoryBudget(Arc<BudgetInner>);
struct BudgetInner {
    maximum: usize,
    used: AtomicUsize,
    released: tokio::sync::Notify,
}
pub struct Reservation {
    budget: MemoryBudget,
    bytes: usize,
}

/// Keeps decoded/encoded expansion charged until the consumer finishes with it.
pub struct ReservedRecord {
    record: Record,
    _reservation: Reservation,
}
impl ReservedRecord {
    pub fn record(&self) -> &Record {
        &self.record
    }
}
impl MemoryBudget {
    pub fn new(maximum: usize) -> Self {
        Self(Arc::new(BudgetInner {
            maximum,
            used: AtomicUsize::new(0),
            released: tokio::sync::Notify::new(),
        }))
    }
    pub fn used(&self) -> usize {
        self.0.used.load(Ordering::Acquire)
    }
    pub fn maximum(&self) -> usize {
        self.0.maximum
    }

    /// Backpressure before transport allocation. The caller supplies an
    /// absolute deadline; cancellation leaves no reservation or waiter behind.
    // `fetch_update` is deprecated for `try_update`, which is unstable on the
    // Rust 1.93 the Linux API image builds with; rename when that image moves.
    #[allow(deprecated)]
    pub async fn reserve_wait(&self, bytes: usize) -> Result<Reservation, Error> {
        if bytes > self.0.maximum {
            return Err(Error::Unavailable);
        }
        loop {
            let released = self.0.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            if self
                .0
                .used
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    used.checked_add(bytes)
                        .filter(|next| *next <= self.0.maximum)
                })
                .is_ok()
            {
                return Ok(Reservation {
                    budget: self.clone(),
                    bytes,
                });
            }
            released.await;
        }
    }
    pub async fn reserve_record_wait(&self, length: usize) -> Result<Reservation, Error> {
        self.reserve_wait(
            length
                .checked_mul(32)
                .and_then(|v| v.checked_add(4096))
                .ok_or(Error::Unavailable)?,
        )
        .await
    }
    // `fetch_update` is deprecated for `try_update`, which is unstable on the
    // Rust 1.93 the Linux API image builds with; rename when that image moves.
    #[allow(deprecated)]
    pub fn reserve(&self, bytes: usize) -> Result<Reservation, Error> {
        self.0
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.0.maximum)
            })
            .map_err(|used| {
                // Sizes only: which cap refused, never what the bytes held.
                // A warning, not a debug line: the session ends on it, and
                // a worker runs at `info`.
                tracing::warn!(
                    requested = bytes,
                    used,
                    maximum = self.0.maximum,
                    "cloud transfer: the accounting cap refused a reservation"
                );
                Error::Unavailable
            })?;
        Ok(Reservation {
            budget: self.clone(),
            bytes,
        })
    }
    /// Covers input, Serde's tagged-enum buffering, decoded byte arrays and
    /// strict field inspection. Reservation precedes the transport allocation.
    /// Deliberately conservative until worker capacity measurements narrow it.
    pub fn reserve_record(&self, length: usize) -> Result<Reservation, Error> {
        self.reserve(
            length
                .checked_mul(32)
                .and_then(|v| v.checked_add(4096))
                .ok_or(Error::Unavailable)?,
        )
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.0.used.fetch_sub(self.bytes, Ordering::AcqRel);
        self.budget.0.released.notify_waiters();
    }
}

#[cfg(test)]
mod tests;
