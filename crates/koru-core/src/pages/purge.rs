// SPDX-License-Identifier: MPL-2.0

//! Permanent document deletion evidence. Unlike recoverable page deletion,
//! any event ends the original document's life. Recovery creates another id.
//! Version bytes are fixed at creation, including absence: transports must
//! choose the bounded payload before writing it to the system document.

use loro::VersionVector;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_ACTOR_BYTES: usize = 128;

/// One immutable event, shared by both carriers. Byte equality is intentional:
/// retries carry exactly the same version encoding, not a re-encoded vector.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub event_id: Uuid,
    pub document_id: Uuid,
    pub root_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Vec<u8>>,
    pub by: String,
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Event")
            .field("event_id", &self.event_id)
            .field("document_id", &self.document_id)
            .field("root_id", &self.root_id)
            .field("version_bytes", &self.version.as_ref().map(Vec::len))
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventError {
    Identity,
    ActorTooLong,
    Version,
}

impl Event {
    /// Construct once, then apply the carrier's complete-message bound before
    /// persisting. No retry may change `version`, including to make it fit.
    pub fn new(
        document_id: Uuid,
        root_id: Uuid,
        version: Option<&VersionVector>,
        by: &str,
    ) -> Self {
        let mut end = by.len().min(MAX_ACTOR_BYTES);
        while !by.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            event_id: Uuid::new_v4(),
            document_id,
            root_id,
            version: version.map(VersionVector::encode),
            by: by[..end].to_owned(),
        }
    }

    pub fn validate(&self) -> Result<(), EventError> {
        let v4 = |id: Uuid| id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122;
        let document = |id: Uuid| {
            matches!(id.get_version_num(), 4 | 5) && id.get_variant() == uuid::Variant::RFC4122
        };
        if !v4(self.event_id) || !document(self.document_id) || !document(self.root_id) {
            return Err(EventError::Identity);
        }
        if self.by.len() > MAX_ACTOR_BYTES {
            return Err(EventError::ActorTooLong);
        }
        if let Some(version) = &self.version {
            VersionVector::decode(version).map_err(|_| EventError::Version)?;
        }
        Ok(())
    }
}

/// `Unknown` is deliberately different from an empty known vector. A device
/// holding content must ask before discarding it when coverage is unknown.
#[derive(Clone, Debug, PartialEq)]
pub enum Coverage {
    Known(VersionVector),
    Unknown,
}

impl Coverage {
    /// Join all evidence for one document; one absent or invalid version makes
    /// the whole coverage unknown. No events means no permanent deletion.
    pub fn of<'a>(events: impl IntoIterator<Item = &'a Event>) -> Option<Self> {
        let mut seen = false;
        let mut joined = VersionVector::new();
        for event in events {
            seen = true;
            let Some(bytes) = &event.version else {
                return Some(Self::Unknown);
            };
            let Ok(version) = VersionVector::decode(bytes) else {
                return Some(Self::Unknown);
            };
            joined.merge(&version);
        }
        seen.then_some(Self::Known(joined))
    }

    pub fn covers(&self, version: &VersionVector) -> bool {
        matches!(self, Self::Known(known) if known.includes_vv(version))
    }
}

/// An exact replay is harmless; a reused identity with a different payload is
/// reported and never overwrites the payload the system document holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Insert {
    Inserted,
    Present,
    Conflict,
}

#[cfg(test)]
mod tests;
