// SPDX-License-Identifier: MPL-2.0

//! What to do about a refusal, and how long to wait before doing it.
//!
//! Two decisions, kept apart because they are different questions. *What* is
//! read off the contract's §7 registry, which already classes every code as a
//! statement about the thing carried or about the session; *how long* is one
//! bounded exponential with jitter whose ceiling is derived rather than
//! chosen.
//!
//! The rule the plan's §12 states and this file is the whole of:
//! **authentication, epoch, incarnation, malformed-data and checksum failures
//! do not retry blindly.** A client that answers every refusal with "wait and
//! try again" turns a fenced account into a device that hammers a service it
//! will never be admitted to.

use std::time::Duration;

use koru_cloud_blob::BlobCode;
use koru_store_core::errors::{Class, Code};

use crate::ports::TransferError;

/// What a refusal means for the next step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// Transient. Wait out the backoff and do the same thing again.
    Retry,
    /// The capability is outside its window. **Nothing is discarded** — ask
    /// the control plane for a fresh one for the *same* session and carry on
    /// from the same offset (plan §13).
    Renew,
    /// The client and the worker disagree about how much is on the disk. Read
    /// the worker's `HEAD` — the authority — and continue from there. Never a
    /// local guess, and never a restart: the bytes already sent are still
    /// there.
    Resync,
    /// The session on the server is terminal. Allocate a new one; the local
    /// object has not moved.
    Restart,
    /// A statement about **this blob**. It stops and its neighbours carry on.
    /// A trigger may bring it back; a timer never does.
    ParkUnit,
    /// A statement about **the session** — authorization moved, the account
    /// is gone, the policy refuses. Everything for this Workspace waits for
    /// the trigger that changes it.
    ParkSession,
}

/// The one decision table.
pub fn classify(error: &TransferError) -> Disposition {
    match error {
        // "There was no server" is not "the server said no".
        TransferError::Unreachable => Disposition::Retry,
        TransferError::NotPermitted(_) | TransferError::NoAccess => Disposition::ParkSession,
        // About **this blob**, and about this device's disk. A smaller
        // picture beside it may still fit, so the pass carries on. A trigger
        // brings it back when space returns; a timer never would, because a
        // disk that filled is not a disk that empties on a schedule.
        TransferError::InsufficientStorage => Disposition::ParkUnit,
        // A control plane that returned somewhere we do not send files is a
        // loud, session-wide stop. Retrying would only ask it again.
        TransferError::Origin(_) => Disposition::ParkSession,
        // The Store's own classes already divide "about this document" from
        // "about the Store", and blobs do not get a second policy — the
        // document pass's `deferrable` is the same line drawn once.
        // The local bytes do not hash to their own name. `Class::Unavailable`
        // is right for a Store that cannot serve and wrong for this: the same
        // read will fail the same way forever, and the Store has already
        // copied the damaged file aside. One named exception rather than a
        // second class, because the class is the document pass's and this is
        // the one code whose remedy is not "try again".
        TransferError::Local(Code::BlobChecksumMismatch) => Disposition::ParkUnit,
        TransferError::Local(code) => match code.class() {
            Class::NotFound | Class::Invalid | Class::Conflict => Disposition::ParkUnit,
            Class::Unavailable => Disposition::Retry,
            Class::Unauthenticated | Class::Forbidden | Class::Internal => Disposition::ParkSession,
        },
        TransferError::Refused(code) => match code {
            // Retryable by the contract's own word.
            BlobCode::StorageUnavailable => Disposition::Retry,

            BlobCode::CapabilityExpired => Disposition::Renew,
            BlobCode::RangeConflict => Disposition::Resync,
            BlobCode::UploadExpired => Disposition::Restart,

            // About the thing carried. Retrying sends the same bytes or the
            // same header and gets the same answer.
            BlobCode::MalformedRange
            | BlobCode::LengthMismatch
            | BlobCode::HashMismatch
            | BlobCode::WrongObject
            | BlobCode::ObjectMissing
            | BlobCode::ObjectTooLarge
            | BlobCode::CloudPolicyUnavailable
            | BlobCode::CloudPlanRequired
            | BlobCode::CloudDocumentCountLimit
            | BlobCode::CloudStoredDocumentBytesLimit
            | BlobCode::CloudPhysicalBytesLimit
            | BlobCode::CloudStagingBytesLimit
            | BlobCode::CloudRemovalCapacityLimit
            | BlobCode::FileExceedsPlanAllowance
            | BlobCode::UsageUnknown
            | BlobCode::AccountOverQuota
            | BlobCode::RelayPathRefused => Disposition::ParkUnit,

            // Authorization has moved. The engine waits for the trigger that
            // says it moved back, which is exactly the enablement or mapping
            // change §10 already lists.
            BlobCode::WrongRegion
            | BlobCode::WrongWorkspace
            | BlobCode::StaleIncarnation
            | BlobCode::StaleEpoch
            | BlobCode::CapabilityInvalid
            | BlobCode::ModeRefusesTransport => Disposition::ParkSession,
        },
    }
}

/// The first wait. Short enough that a single lost packet costs nothing a
/// person notices.
pub const MIN_BACKOFF: Duration = Duration::from_secs(1);

/// The ceiling, **derived**: the Agent's coordinator heartbeat is 30 seconds
/// and is what reschedules a pass anyway, so a blob retry that waited longer
/// than one would be a timer outliving the thing that would have retried it
/// for free — and a device kept awake for nothing.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

const _: () = assert!(MIN_BACKOFF.as_millis() < MAX_BACKOFF.as_millis());

/// The waits for a blob the Cloud has never held. Nothing this device does
/// changes that answer: the bytes arrive when the device holding them is
/// next online and uploads, and asking every few seconds in the meantime
/// is a request per repaint for something that is not there. Seen on the
/// server as one workspace asking for one object every seven seconds.
pub const MISSING_MIN_BACKOFF: Duration = Duration::from_secs(60);
pub const MISSING_MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);

const _: () = assert!(MISSING_MIN_BACKOFF.as_millis() < MISSING_MAX_BACKOFF.as_millis());

/// Bounded exponential with jitter.
///
/// The jitter is **full** rather than a fraction: two devices that both lost
/// the same network come back at the same instant, and a ±10% spread around
/// one number is still one number. Uniform over `[min, backoff]` costs one
/// random word and turns a thundering herd into a trickle.
#[derive(Clone, Copy, Debug, Default)]
pub struct Backoff {
    failures: u32,
}

impl Backoff {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a failure and answer with how long to wait.
    pub fn failed(&mut self) -> Duration {
        self.failed_within(MIN_BACKOFF, MAX_BACKOFF)
    }
    /// The same bounded exponential over a caller's bounds, for the one
    /// wait that is measured in minutes rather than seconds.
    pub fn failed_within(&mut self, min: Duration, max: Duration) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let ceiling = min
            .checked_mul(1u32 << self.failures.min(6))
            .unwrap_or(max)
            .min(max);
        jitter(min, ceiling)
    }

    /// Progress. Consecutive means consecutive.
    pub fn cleared(&mut self) {
        self.failures = 0;
    }

    pub fn failures(self) -> u32 {
        self.failures
    }
}

fn jitter(low: Duration, high: Duration) -> Duration {
    let (low, high) = (low.as_millis() as u64, high.as_millis() as u64);
    if high <= low {
        return Duration::from_millis(low);
    }
    let mut word = [0u8; 8];
    // A failure to read the OS generator is not a reason to fail a retry; the
    // ceiling is then the wait, which is the conservative direction.
    if rand_core::RngCore::try_fill_bytes(&mut rand_core::OsRng, &mut word).is_err() {
        return Duration::from_millis(high);
    }
    let span = high - low + 1;
    Duration::from_millis(low + (u64::from_le_bytes(word) % span))
}

#[cfg(test)]
mod tests;
