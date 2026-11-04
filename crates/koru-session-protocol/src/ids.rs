// SPDX-License-Identifier: MPL-2.0

use std::{fmt, str::FromStr};

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use uuid::{Uuid, Variant, Version};

/// Why a serialized UUID was refused.
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

fn uuid_schema() -> Schema {
    schemars::json_schema!({
        "type": "string",
        "format": "uuid",
        "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
    })
}

macro_rules! uuid_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        uuid_id!($(#[$doc])* $name, $prefix, stringify!($name));
    };
    ($(#[$doc:meta])* $name:ident, $prefix:literal, $schema_name:expr) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            /// Mint an opaque v4 UUID. IDs are never derived from names or paths.
            pub fn mint() -> Self { Self(Uuid::new_v4()) }

            pub fn as_uuid(self) -> Uuid { self.0 }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                parse_v4(value).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({:.8})", $prefix, self.0.as_hyphenated())
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where S: Serializer {
                serializer.serialize_str(&self.0.hyphenated().to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where D: Deserializer<'de> {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(de::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed($schema_name)
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema { uuid_schema() }
        }
    };
}

uuid_id!(
    /// One live terminal or managed-agent session.
    SessionId,
    "session",
    "TerminalSessionId"
);
uuid_id!(
    /// One custom launch profile. Built-ins use stable token keys.
    SessionProfileId,
    "session-profile"
);
uuid_id!(
    /// One session-owned attachment.
    AttachmentId,
    "attachment"
);
uuid_id!(
    /// One logical managed-agent turn, minted by Koru.
    AgentTurnId,
    "agent-turn"
);
uuid_id!(
    /// One retry-safe mutating command.
    IdempotencyKey,
    "idempotency"
);
uuid_id!(
    /// One provider-neutral approval request.
    ApprovalId,
    "approval"
);
uuid_id!(
    /// One offered option, scoped by its pending approval.
    ApprovalChoiceId,
    "approval-choice"
);
uuid_id!(
    /// One set of questions an agent asked (protocol v21, koru-ai D108),
    /// minted by the adapter; the agent's own request id never crosses.
    QuestionRequestId,
    "question-request"
);
uuid_id!(
    /// One provider-neutral managed tool call.
    ToolCallId,
    "tool-call"
);
uuid_id!(
    /// The Koru AI chat a session belongs to (protocol v16, `SessionOrigin`).
    /// The chat's own id, which the AI worker mints as a v4 UUID; the host
    /// compares it and never derives anything from it.
    ChatOriginId,
    "chat-origin"
);
uuid_id!(
    /// One device-local terminal project. Minted by the client that creates
    /// the project, which is what makes `CreateProject` replay-safe.
    ProjectId,
    "project"
);
uuid_id!(
    /// A Koru catalog profile, as a project's scope names it.
    ///
    /// This is the persona a Koru window selects in its banner, **not** a
    /// launch profile ([`SessionProfileId`]). It is never minted here: the
    /// value is copied from the catalog the window already read.
    CatalogProfileId,
    "catalog-profile"
);
uuid_id!(
    /// A Koru catalog workspace, as a project's scope names it. Never minted
    /// here; copied from the catalog the window already read.
    CatalogWorkspaceId,
    "catalog-workspace"
);

/// Secret proving exclusive controller ownership. It is intentionally absent
/// from summaries and redacted from diagnostics.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ControllerLeaseToken(Uuid);

impl ControllerLeaseToken {
    pub fn mint() -> Self {
        Self(Uuid::new_v4())
    }
}

impl FromStr for ControllerLeaseToken {
    type Err = IdParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_v4(value).map(Self)
    }
}

impl fmt::Debug for ControllerLeaseToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ControllerLeaseToken(<redacted>)")
    }
}

impl Serialize for ControllerLeaseToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.hyphenated().to_string())
    }
}

impl<'de> Deserialize<'de> for ControllerLeaseToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

impl JsonSchema for ControllerLeaseToken {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("ControllerLeaseToken")
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        uuid_schema()
    }
}
