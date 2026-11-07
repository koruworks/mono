// SPDX-License-Identifier: MPL-2.0

use super::{Error, MAX_CONTROL, MAX_PAYLOAD, RESTRICTED_LIMIT};
use crate::{
    cloud::{Bytes, Version1},
    protocol::PeerFrame,
};
use koru_device::cloud::CloudId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Unit {
    Document {
        workspace_id: CloudId,
        document_id: CloudId,
    },
    Identity {
        profile_id: CloudId,
    },
    Private {
        profile_id: CloudId,
    },
    Personal {
        profile_id: CloudId,
        workspace_id: CloudId,
    },
}
impl Unit {
    pub(super) fn valid(self) -> bool {
        let valid = |id: CloudId| id.as_bytes()[6] >> 4 == 4 && id.as_bytes()[8] >> 6 == 2;
        match self {
            Self::Document {
                workspace_id,
                document_id,
            } => valid(workspace_id) && valid(document_id),
            Self::Identity { profile_id } | Self::Private { profile_id } => valid(profile_id),
            Self::Personal {
                profile_id,
                workspace_id,
            } => valid(profile_id) && valid(workspace_id),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Accept,
    Defer,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Start {
    pub version: Version1,
    pub policy_generation: i64,
    pub unit: Unit,
    pub batch_id: [u8; 8],
    pub total_length: usize,
    pub fragment_count: usize,
    pub sha256: Bytes<32>,
    /// The upload's claim reference (`cloud-sync-admission`, the app
    /// connection amendment; TECHNICAL §5.4 item 6): `SHA-256` of the claim
    /// handle the call that made this upload presented. Written only when
    /// present, and last; `null` is refused, because absence is the only way
    /// to say none. Only an app connection's client sends one, and only the
    /// worker that admitted that connection receives one ([`Stream`] refuses
    /// it on every other session), so a device's start is byte-identical to
    /// before. Never logged: `Debug` omits it.
    ///
    /// [`Stream`]: super::Stream
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub claim: Option<Bytes<32>>,
}
/// `claim` when the property is there: a value, never `null`.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Bytes<32>>, D::Error> {
    Bytes::deserialize(d).map(Some)
}
impl Start {
    pub fn validate(&self) -> Result<(), Error> {
        if self.policy_generation <= 0
            || !self.unit.valid()
            || !(1..=MAX_PAYLOAD).contains(&self.total_length)
            || !(1..=16384).contains(&self.fragment_count)
            || self.fragment_count > self.total_length
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
impl std::fmt::Debug for Start {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Start")
            .field("bytes", &self.total_length)
            .field("fragments", &self.fragment_count)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    TransferPolicy {
        version: Version1,
        policy_generation: i64,
        automatic_limit_bytes: usize,
    },
    TransferStart(Start),
    TransferDecision {
        version: Version1,
        policy_generation: i64,
        unit: Unit,
        batch_id: [u8; 8],
        sha256: Bytes<32>,
        decision: Decision,
    },
}
impl Control {
    pub fn policy(generation: i64, limit: usize) -> Self {
        Self::TransferPolicy {
            version: Version1,
            policy_generation: generation,
            automatic_limit_bytes: limit,
        }
    }
    pub fn decision(start: &Start, decision: Decision) -> Self {
        Self::TransferDecision {
            version: Version1,
            policy_generation: start.policy_generation,
            unit: start.unit,
            batch_id: start.batch_id,
            sha256: start.sha256,
            decision,
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::TransferPolicy {
                policy_generation,
                automatic_limit_bytes,
                ..
            } if *policy_generation > 0
                && [0, RESTRICTED_LIMIT, MAX_PAYLOAD].contains(automatic_limit_bytes) =>
            {
                Ok(())
            }
            Self::TransferStart(start) => start.validate(),
            Self::TransferDecision {
                policy_generation,
                unit,
                ..
            } if *policy_generation > 0 && unit.valid() => Ok(()),
            _ => Err(Error::Invalid),
        }
    }
}

#[derive(Clone)]
pub enum Record {
    Control(Control),
    Peer(PeerFrame),
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Control(<redacted>)")
    }
}
impl std::fmt::Debug for Record {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Record(<redacted>)")
    }
}

/// Validate all five header bytes before the transport allocates payload.
pub fn payload_length(length: u32, tag: u8) -> Result<usize, Error> {
    let cap = match tag {
        0 => MAX_PAYLOAD,
        1 => MAX_CONTROL,
        _ => return Err(Error::Invalid),
    };
    let payload = (length as usize).checked_sub(1).ok_or(Error::Invalid)?;
    if payload == 0 || payload > cap {
        return Err(Error::Invalid);
    }
    Ok(payload)
}
pub fn decode_control(bytes: &[u8]) -> Result<Control, Error> {
    if bytes.is_empty() || bytes.len() > MAX_CONTROL {
        return Err(Error::Invalid);
    }
    let value: Control = serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?;
    value.validate()?;
    Ok(value)
}
pub fn decode_record(tag: u8, bytes: &[u8]) -> Result<Record, Error> {
    payload_length(
        u32::try_from(bytes.len())
            .map_err(|_| Error::Invalid)?
            .checked_add(1)
            .ok_or(Error::Invalid)?,
        tag,
    )?;
    match tag {
        1 => Ok(Record::Control(decode_control(bytes)?)),
        0 => {
            strict_peer::validate(bytes)?;
            Ok(Record::Peer(
                serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?,
            ))
        }
        _ => Err(Error::Invalid),
    }
}
pub fn encode_record(value: &Record) -> Result<Vec<u8>, Error> {
    use std::io::{self, Write};
    struct Bounded {
        bytes: Vec<u8>,
        cap: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.cap - self.bytes.len() {
                return Err(io::Error::other("Cloud record bound"));
            }
            self.bytes
                .try_reserve(bytes.len())
                .map_err(|_| io::Error::other("Cloud record allocation"))?;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let (tag, cap) = match value {
        Record::Control(_) => (1, MAX_CONTROL),
        Record::Peer(_) => (0, MAX_PAYLOAD),
    };
    let mut writer = Bounded {
        bytes: vec![0, 0, 0, 0, tag],
        cap: cap + 5,
    };
    match value {
        Record::Control(v) => {
            v.validate()?;
            serde_json::to_writer(&mut writer, v)
        }
        Record::Peer(v) => serde_json::to_writer(&mut writer, v),
    }
    .map_err(|_| Error::Invalid)?;
    let length = (writer.bytes.len() - 4) as u32;
    payload_length(length, tag)?;
    writer.bytes[..4].copy_from_slice(&length.to_be_bytes());
    Ok(writer.bytes)
}

mod strict_peer;
