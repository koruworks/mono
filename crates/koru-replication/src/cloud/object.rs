// SPDX-License-Identifier: MPL-2.0

//! Disconnected Cloud object content verification. No storage or authority port.
//!
//! A checksum, matching expected metadata and even a complete Loro proof do not
//! establish current authorization or a durable receipt. Callers must supply
//! explicit limits; no unmeasured checkpoint default is installed here.

mod causal;
mod codec;
mod identity;

pub use causal::{
    CheckpointOrigin, ContentProof, HeadCommitment, RestoredContent, StagedObject, restore_chain,
    stage_initial_checkpoint, starts_from_empty, verify_chain,
};
pub use codec::{decode, encode, preflight_length};
pub use identity::{StagingName, Unit, logical_key, staging_name};

use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1};
use sha2::{Digest, Sha256};

pub type Hash = [u8; 32];
const PREFIX_BYTES: usize = 22;
const HEADER_MAX: u32 = 4 * 1024 * 1024;
const VECTOR_MAX: u32 = 1024 * 1024;
const KEY_MAX: usize = 512;
const STAGING_MAX: usize = 768;
// Signed review upper bound only. Not an approved capacity or installed default.
const CHECKPOINT_REVIEW_MAX: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLimits,
    TooLarge,
    Integer,
    Truncated,
    Length,
    Magic,
    Version,
    Tag,
    Region,
    Unit,
    CausalShape,
    Checksum,
    Binding,
    Key,
    Loro,
    Ancestors,
    Predecessor,
    Result,
    Frontier,
    Schema,
    Scope,
    Chain,
    Allocation,
}
impl Error {
    /// Only the signed bound refusal has a public code in this primitive.
    /// Other typed content failures require the later owning adapter's mapping.
    pub fn public_bound_code(self) -> Option<&'static str> {
        (self == Self::TooLarge).then_some("cloud_object_too_large")
    }
}

/// Serialized-input limits only, not a Loro expanded-memory/capacity guarantee.
/// There is deliberately no Default. Explicit checkpoint limits cannot exceed
/// the signed review ceiling, which is not an approved deployment value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    header: u32,
    vector: u32,
    segment: u64,
    checkpoint: u64,
}
impl Limits {
    /// Fit every valid envelope whose individual vectors fit `vector`.
    /// Header bytes include two vectors and fixed metadata; using the vector
    /// ceiling as the whole header ceiling rejects otherwise valid documents.
    pub fn for_vector_capacity(vector: u32, segment: u64, checkpoint: u64) -> Result<Self, Error> {
        let header = codec::header_bound(vector).ok_or(Error::InvalidLimits)?;
        Self::new(header, vector, segment, checkpoint)
    }

    pub fn checkpoint_limit(self) -> u64 {
        self.checkpoint
    }
    pub fn segment_limit(self) -> u64 {
        self.segment
    }
    pub fn vector_limit(self) -> u32 {
        self.vector
    }

    pub fn new(header: u32, vector: u32, segment: u64, checkpoint: u64) -> Result<Self, Error> {
        if header == 0
            || header > HEADER_MAX
            || vector == 0
            || vector > VECTOR_MAX
            || segment == 0
            || segment > super::transfer::MAX_PAYLOAD as u64
            || checkpoint == 0
            || checkpoint > CHECKPOINT_REVIEW_MAX
        {
            return Err(Error::InvalidLimits);
        }
        Ok(Self {
            header,
            vector,
            segment,
            checkpoint,
        })
    }
    fn payload(self, kind: Kind, length: u64) -> Result<(), Error> {
        let maximum = match kind {
            Kind::Segment => self.segment,
            Kind::Checkpoint => self.checkpoint,
        };
        if length > maximum {
            tracing::warn!(
                check = "object_payload",
                object_kind = ?kind,
                actual_bytes = length,
                limit_bytes = maximum,
                "cloud object: size limit rejected payload"
            );
            Err(Error::TooLarge)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Checkpoint,
    Segment,
}

/// Correlation values, not verified current authority. Every field is compared
/// independently with the caller's expected object metadata before causal work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub kind: Kind,
    pub dataset: CloudReplicaPrincipalV1,
    pub unit: Unit,
    pub cloud_unit_id: CloudId,
    pub object_id: CloudId,
    pub sequence: u64,
    pub writer_worker_id: CloudId,
    pub writer_lease_id: CloudId,
    pub writer_lease_epoch: u64,
    pub authority_workspace_id: CloudId,
    pub authorization_version: u64,
    pub membership_epoch: u64,
    pub cloud_access_epoch: u64,
    pub covers_through_sequence: u64,
}
impl Metadata {
    fn validate(self) -> Result<(), Error> {
        self.unit.validate(self.dataset)?;
        for value in [
            self.sequence,
            self.writer_lease_epoch,
            self.authorization_version,
            self.membership_epoch,
            self.cloud_access_epoch,
        ] {
            integer(value, true)?;
        }
        integer(self.covers_through_sequence, false)?;
        match self.kind {
            Kind::Segment if self.covers_through_sequence != 0 => Err(Error::CausalShape),
            Kind::Checkpoint if self.covers_through_sequence >= self.sequence => {
                Err(Error::CausalShape)
            }
            _ => Ok(()),
        }
    }
}

/// Exact captured opaque bytes. They are never normalized or re-encoded to
/// manufacture a matching receipt digest. Debug deliberately omits content.
#[derive(Clone, Copy)]
pub struct Body<'a> {
    pub predecessor_version: &'a [u8],
    pub result_version: &'a [u8],
    pub checkpoint_frontiers: &'a [u8],
    pub payload: &'a [u8],
}
impl std::fmt::Debug for Body<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Body")
            .field("payload_bytes", &self.payload.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commitments {
    pub unit_ref_digest: Hash,
    pub predecessor_version_digest: Option<Hash>,
    pub result_version_digest: Hash,
    pub checkpoint_frontier_digest: Option<Hash>,
    pub payload_sha256: Hash,
    pub object_sha256: Hash,
    pub request_digest: Hash,
    pub payload_byte_length: u64,
    pub object_byte_length: u64,
}

/// The owner must derive this independently from its expected intent/receipt,
/// never by copying the untrusted decoded object to make a comparison pass.
#[derive(Clone, Copy, Debug)]
pub struct ExpectedObject<'a> {
    pub metadata: Metadata,
    pub logical_key: &'a str,
    pub commitments: Commitments,
}

#[derive(Clone, Copy, Debug)]
pub struct Decoded<'a> {
    metadata: Metadata,
    body: Body<'a>,
    commitments: Commitments,
}
impl<'a> Decoded<'a> {
    pub fn metadata(&self) -> Metadata {
        self.metadata
    }
    pub fn body(&self) -> Body<'a> {
        self.body
    }
    pub fn commitments(&self) -> Commitments {
        self.commitments
    }
    pub fn bind(self, expected: ExpectedObject<'_>) -> Result<BoundObject<'a>, Error> {
        if self.metadata != expected.metadata || self.commitments != expected.commitments {
            return Err(Error::Binding);
        }
        if expected.logical_key.len() > KEY_MAX {
            return Err(Error::TooLarge);
        }
        if !expected.logical_key.is_ascii() || expected.logical_key != logical_key(self.metadata)? {
            return Err(Error::Key);
        }
        Ok(BoundObject(self))
    }
}

/// Byte/metadata correlation only. Not a SQL receipt, authorization or lease.
#[derive(Clone, Copy, Debug)]
pub struct BoundObject<'a>(Decoded<'a>);
impl<'a> BoundObject<'a> {
    pub fn metadata(&self) -> Metadata {
        self.0.metadata
    }
    pub fn body(&self) -> Body<'a> {
        self.0.body
    }
    pub fn commitments(&self) -> Commitments {
        self.0.commitments
    }
}

fn hash(bytes: &[u8]) -> Hash {
    Sha256::digest(bytes).into()
}
fn integer(value: u64, positive: bool) -> Result<(), Error> {
    if value > i64::MAX as u64 || (positive && value == 0) {
        Err(Error::Integer)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
