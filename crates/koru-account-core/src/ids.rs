// SPDX-License-Identifier: MPL-2.0

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use uuid::{Uuid, Variant, Version};

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

#[cfg(feature = "schema")]
fn uuid_v4_schema() -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "format": "uuid",
        "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
    })
}

macro_rules! account_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            /// Mint an opaque random identifier. No client value, name, email,
            /// provider subject, or clock contributes to it.
            pub fn mint() -> Self { Self(Uuid::new_v4()) }

            pub fn as_uuid(self) -> Uuid { self.0 }
        }

        impl Default for $name {
            fn default() -> Self { Self::mint() }
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

        #[cfg(feature = "schema")]
        impl schemars::JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($name))
            }

            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                uuid_v4_schema()
            }
        }
    };
}

account_id!(
    /// The durable logical Koru account. Distinct from every local Profile ID.
    KoruIdentityId,
    "identity"
);
account_id!(
    /// One provider credential identity attached to a Koru Identity.
    AuthIdentityId,
    "auth-identity"
);
account_id!(
    /// One revocable Koru account session, never a provider token.
    AccountSessionId,
    "account-session"
);
account_id!(
    /// One expiring, single-purpose authentication or linking flow.
    AuthFlowId,
    "auth-flow"
);
account_id!(
    /// One approved connection of a third-party application, such as an AI
    /// assistant, to a Koru account. Each approval mints its own.
    AppConnectionId,
    "app-connection"
);
