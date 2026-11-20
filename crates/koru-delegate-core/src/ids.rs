// SPDX-License-Identifier: MPL-2.0

//! The identities a delegated run mints.
//!
//! Every id is a **client-minted v4 UUID**, never derived from a name, a path,
//! a machine or an objective (ADR-39). Their `Debug` is a prefix and eight hex
//! characters, because ids appear in test failures constantly and a full UUID
//! drowns the message. The ids this crate *speaks* but does not own —
//! `DocumentId`, `DeviceId`, `AgentTurnId`, `EventId` — come from the leaf
//! crates that own them (see the crate manifest).

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use uuid::{Uuid, Variant, Version};

/// Why a serialized id was refused. The same contract as the session
/// protocol's: canonical lowercase hyphenated spelling, version 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdParseError {
    InvalidSpelling,
    NotVersion4,
}

impl fmt::Display for IdParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidSpelling => "expected a canonical lowercase hyphenated UUID",
            Self::NotVersion4 => "expected a version 4 UUID",
        })
    }
}

impl std::error::Error for IdParseError {}

fn parse_v4(value: &str) -> Result<Uuid, IdParseError> {
    let parsed = Uuid::parse_str(value).map_err(|_| IdParseError::InvalidSpelling)?;
    if parsed.hyphenated().to_string() != value {
        return Err(IdParseError::InvalidSpelling);
    }
    if parsed.get_version() != Some(Version::Random) || parsed.get_variant() != Variant::RFC4122 {
        return Err(IdParseError::NotVersion4);
    }
    Ok(parsed)
}

macro_rules! uuid_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            /// Mint an opaque v4 UUID. Never derived from a name or a path.
            pub fn mint() -> Self {
                Self(Uuid::new_v4())
            }

            /// Wrap a UUID minted elsewhere — a fixture, a database row, a
            /// wire value already validated.
            pub fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            pub fn as_uuid(self) -> Uuid {
                self.0
            }

            /// The one spelling stored anywhere: hyphenated and lowercase.
            pub fn to_storage(self) -> String {
                self.0.hyphenated().to_string()
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                parse_v4(value).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.hyphenated().fmt(f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let full = self.0.simple().to_string();
                write!(f, "{}({})", $prefix, &full[..8])
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.to_storage())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(de::Error::custom)
            }
        }

        /// Written by hand because the serde above is: a derive would
        /// describe the `Uuid` inside rather than the string that crosses.
        #[cfg(feature = "schema")]
        impl schemars::JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($name))
            }

            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({
                    "type": "string",
                    "format": "uuid",
                    "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
                })
            }
        }
    };
}

uuid_id!(/// A user-created Bot, independent of its conversations and Runs.
    BotId, "Bot");
uuid_id!(/// One stable Bot command identity, retained across lost replies.
    BotRequestId, "BotRequest");

uuid_id!(
    /// One delegated run, from the creation intent to the last rating.
    ///
    /// Minted by the client that asks (§7.1: the intent, the Run document and
    /// the reconciliation all key on it), so a retried request is the same
    /// run and never a second one.
    RunId,
    "run"
);
uuid_id!(
    /// One attempt to execute a run. A resume is a fresh attempt (§7.6), and
    /// every turn is keyed under the attempt it belongs to (§7.5).
    RunAttemptId,
    "attempt"
);
uuid_id!(
    /// One step in a run's history (D45).
    TimelineEntryId,
    "step"
);
uuid_id!(
    /// The stable identity of one turn's usage accounting (§17.2). A retry
    /// repeats it; a correction keeps it and raises the revision.
    UsageFactId,
    "usage"
);
uuid_id!(
    /// A reference to retained provider/model metadata (§17.3). An `Id`, so
    /// it can ride an event attribute where a model *name* cannot, and so it
    /// is dropped mechanically at the telemetry boundary (D41).
    ModelRef,
    "model"
);
uuid_id!(
    /// One immutable artifact version. Its content is a `BlobHash`; this is
    /// its identity (P1.10 separates the two). Minted here so a
    /// [`crate::ResultReference`] can name a result before the artifact
    /// service exists; `koru-artifact-core` speaks this id rather than a
    /// second one.
    ArtifactVersionId,
    "artifact"
);
uuid_id!(
    /// The node a block-level context item pins, carried as its UUID.
    ///
    /// The node model's own id lives in `koru-core`, which this crate must
    /// not link: the orchestrator has no node model and must not acquire one
    /// to name a paragraph (the `koru-link` precedent). The value *is* that
    /// node's UUID; only the type is local.
    BlockNodeId,
    "node"
);
uuid_id!(
    /// One schedule — work that repeats (M2). A slot is
    /// `(ScheduleId, occurrence instant)` and never the revision (§7.7), so
    /// editing a template can never turn one slot into two. Minted here
    /// because `runs.db` claims slots before the schedule record (P2.2)
    /// exists; the record speaks this id rather than a second one.
    ScheduleId,
    "schedule"
);
uuid_id!(
    /// One deliberate schedule ownership transfer (D33).
    HandoffId,
    "handoff"
);
uuid_id!(
    /// One signed-in account at one vendor, owned by a person on one computer
    /// (M5). The AI owner mints it; here it is only a reference inside a grant,
    /// a decision binding and an effect row. It names no account and no secret.
    ConnectionId,
    "connection"
);
uuid_id!(
    /// One item a person picked on a connection (M5). Koru's own id, never the
    /// vendor's reference, which stays with the AI owner.
    ConnectionItemId,
    "connection-item"
);
uuid_id!(
    /// One process that may call a vendor for a private operation (M5). Minted
    /// once per process and never persisted by that process; the ledger keeps
    /// it on each effect row so a later process can close what this one left.
    PrivateDispatcherId,
    "dispatcher"
);
uuid_id!(
    /// One request a Bot Run made of another Bot (M6). Minted by the AI owner
    /// with the child Run's id; the stable name of the request everywhere.
    ChildRequestId,
    "child-request"
);

/// Which attempt of a run is current — the fence §7.5 and §7.6 rely on.
///
/// A monotonic counter per run, starting at [`AttemptGeneration::FIRST`]. An
/// event, an MCP call or a session lookup carrying an older generation may
/// not touch status, control or authority; see
/// [`crate::AttemptGeneration::admits`]. It is a counter rather than an id so
/// "older" is a comparison, not a lookup.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct AttemptGeneration(u32);

impl AttemptGeneration {
    /// The generation of a run's first attempt.
    pub const FIRST: Self = Self(1);

    /// The generation a resume mints. Saturates rather than wrapping: a run
    /// resumed four billion times is a bug elsewhere, and wrapping would turn
    /// the fence inside out.
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    pub fn get(self) -> u32 {
        self.0
    }

    /// A generation read back from storage. Zero was never minted, so it is
    /// refused rather than becoming a generation older than the first.
    pub fn from_raw(value: u32) -> Option<Self> {
        (value > 0).then_some(Self(value))
    }

    /// Whether `offered`, the generation an event or call carries, may touch
    /// this attempt's status, control or authority. Only the current
    /// generation may; an older one is refused with the stable
    /// [`crate::StaleAttempt`], and a newer one is a bug in the caller that is
    /// refused the same way rather than silently admitted.
    pub fn admits(self, offered: Self) -> Result<(), crate::StaleAttempt> {
        if offered == self {
            Ok(())
        } else {
            Err(crate::StaleAttempt {
                current: self,
                offered,
            })
        }
    }
}
