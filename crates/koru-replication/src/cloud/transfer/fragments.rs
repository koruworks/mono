// SPDX-License-Identifier: MPL-2.0

use super::{Error, FRAGMENT_BYTES, MAX_PAYLOAD, MemoryBudget, Reservation, Start, Unit};
use crate::{
    cloud::{Bytes, Version1},
    protocol::PeerFrame,
};
use koru_store_core::WorkspaceId;
use loro_protocol::{BatchId, CrdtType, ProtocolMessage, encoding};
use sha2::{Digest, Sha256};

// A document may be 16 MiB, but its expanded JSON must not monopolize a
// shared host budget. Keep existing 200 KiB fragments and batch them within
// 1 MiB records. This is a sender scheduling choice, not a wire-limit change.
const MAX_BATCH: usize = 1024 * 1024;

/// Lazy producer: retain one raw export, produce only the next bounded frame.
/// This never concatenates all JSON-expanded fragments into a second export.
pub struct Outgoing {
    pub(super) start: Start,
    workspace: WorkspaceId,
    room: String,
    bytes: Vec<u8>,
    next: usize,
    header_sent: bool,
    _reservation: Reservation,
}
impl Outgoing {
    pub(super) fn new(
        unit: Unit,
        workspace: WorkspaceId,
        room: String,
        generation: i64,
        batch: [u8; 8],
        bytes: Vec<u8>,
        claim: Option<[u8; 32]>,
        budget: &MemoryBudget,
    ) -> Result<Self, Error> {
        if bytes.len() > MAX_PAYLOAD {
            tracing::warn!(
                ?unit,
                check = "outgoing_transfer_payload",
                actual_bytes = bytes.len(),
                limit_bytes = MAX_PAYLOAD,
                "cloud transfer: size limit rejected outgoing payload"
            );
            return Err(Error::TooLarge);
        }
        let start = Start {
            version: Version1,
            policy_generation: generation,
            unit,
            batch_id: batch,
            total_length: bytes.len(),
            fragment_count: bytes.len().div_ceil(FRAGMENT_BYTES),
            sha256: Bytes(Sha256::digest(&bytes).into()),
            claim: claim.map(Bytes),
        };
        start.validate()?;
        let reservation = budget.reserve(
            bytes
                .capacity()
                .checked_add(room.capacity())
                .ok_or(Error::Unavailable)?,
        )?;
        Ok(Self {
            start,
            workspace,
            room,
            bytes,
            next: 0,
            header_sent: false,
            _reservation: reservation,
        })
    }
    pub(super) fn finished(&self) -> bool {
        self.header_sent && self.next == self.start.fragment_count
    }
    pub(super) fn next_frame_bound(&self) -> usize {
        // JSON byte arrays cost at most four bytes per encoded byte. Standard
        // messages add a bounded room/header envelope per <=200 KiB chunk.
        let remaining = self.bytes.len().saturating_sub(self.next * FRAGMENT_BYTES);
        (remaining
            .saturating_mul(4)
            .saturating_add((self.start.fragment_count - self.next + 1) * 2048))
        .min(MAX_BATCH)
    }
    pub(super) fn next_frame(&mut self) -> Result<Option<PeerFrame>, Error> {
        if self.finished() {
            return Ok(None);
        }
        let mut messages = Vec::new();
        let mut json_size = serde_json::to_vec(&PeerFrame::LoroBatch {
            workspace_id: self.workspace,
            messages: Vec::new(),
        })
        .map_err(|_| Error::Invalid)?
        .len();
        loop {
            let message = if !self.header_sent {
                ProtocolMessage::DocUpdateFragmentHeader {
                    crdt: CrdtType::Loro,
                    room_id: self.room.clone(),
                    batch_id: BatchId(self.start.batch_id),
                    fragment_count: self.start.fragment_count as u64,
                    total_size_bytes: self.start.total_length as u64,
                }
            } else if self.next < self.start.fragment_count {
                let offset = self.next * FRAGMENT_BYTES;
                ProtocolMessage::DocUpdateFragment {
                    crdt: CrdtType::Loro,
                    room_id: self.room.clone(),
                    batch_id: BatchId(self.start.batch_id),
                    index: self.next as u64,
                    fragment: self.bytes[offset..(offset + FRAGMENT_BYTES).min(self.bytes.len())]
                        .to_vec(),
                }
            } else {
                break;
            };
            let encoded = encoding::encode(&message).map_err(|_| Error::Invalid)?;
            let added = serde_json::to_vec(&encoded)
                .map_err(|_| Error::Invalid)?
                .len()
                + usize::from(!messages.is_empty());
            if json_size + added > MAX_BATCH {
                if messages.is_empty() {
                    tracing::warn!(
                        unit = ?self.start.unit,
                        check = "outgoing_fragment_json",
                        actual_bytes = json_size + added,
                        limit_bytes = MAX_BATCH,
                        payload_bytes = self.start.total_length,
                        fragment_index = self.next,
                        "cloud transfer: size limit rejected encoded fragment"
                    );
                    return Err(Error::TooLarge);
                }
                break;
            }
            json_size += added;
            messages.push(encoded);
            if self.header_sent {
                self.next += 1;
            } else {
                self.header_sent = true;
            }
        }
        Ok(Some(PeerFrame::LoroBatch {
            workspace_id: self.workspace,
            messages,
        }))
    }
}

pub(super) struct Incoming {
    pub start: Start,
    pub room: String,
    pub bytes: Vec<u8>,
    pub deadline: u64,
    next: usize,
    header: bool,
    pub reservation: Reservation,
}
impl Incoming {
    pub fn new(
        start: Start,
        room: String,
        deadline: u64,
        budget: &MemoryBudget,
    ) -> Result<Self, Error> {
        let reservation = budget.reserve(
            start
                .total_length
                .checked_add(room.capacity())
                .ok_or(Error::Unavailable)?,
        )?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(start.total_length)
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            start,
            room,
            bytes,
            deadline,
            next: 0,
            header: false,
            reservation,
        })
    }
    pub fn complete(&self) -> bool {
        self.header && self.next == self.start.fragment_count
    }
    pub fn frame(&mut self, messages: Vec<Vec<u8>>) -> Result<bool, Error> {
        if messages.is_empty() || self.complete() {
            return Err(Error::Invalid);
        }
        for encoded in messages {
            if self.complete() {
                return Err(Error::Invalid);
            }
            let msg = encoding::try_decode(&encoded).ok_or(Error::Invalid)?;
            // Reject trailing protocol bytes and noncanonical fragment encoding:
            // try_decode alone is not necessarily a consumes-all decoder.
            if encoding::encode(&msg).map_err(|_| Error::Invalid)? != encoded {
                return Err(Error::Invalid);
            }
            match msg {
                ProtocolMessage::DocUpdateFragmentHeader {
                    crdt: CrdtType::Loro,
                    room_id,
                    batch_id,
                    fragment_count,
                    total_size_bytes,
                } if !self.header
                    && room_id == self.room
                    && batch_id.0 == self.start.batch_id
                    && fragment_count == self.start.fragment_count as u64
                    && total_size_bytes == self.start.total_length as u64 =>
                {
                    self.header = true
                }
                ProtocolMessage::DocUpdateFragment {
                    crdt: CrdtType::Loro,
                    room_id,
                    batch_id,
                    index,
                    fragment,
                } if self.header
                    && room_id == self.room
                    && batch_id.0 == self.start.batch_id
                    && index == self.next as u64
                    && !fragment.is_empty()
                    && fragment.len() <= self.start.total_length - self.bytes.len() =>
                {
                    self.bytes.extend(fragment);
                    self.next += 1;
                }
                _ => return Err(Error::Invalid),
            }
        }
        if self.complete() {
            let hash: [u8; 32] = Sha256::digest(&self.bytes).into();
            if self.bytes.len() != self.start.total_length || hash != self.start.sha256.0 {
                return Err(Error::Invalid);
            }
        }
        Ok(self.complete())
    }
}
