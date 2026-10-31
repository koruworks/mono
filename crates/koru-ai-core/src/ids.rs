// SPDX-License-Identifier: MPL-2.0

//! Stable Koru AI identities.
//!
//! Every value minted here is a UUID v4. Parsing accepts any valid UUID for
//! forward compatibility with the Store identity convention; creation never
//! derives identity from content, a path, another id, a timestamp, or a row.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub const fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }

            pub fn parse(value: &str) -> Option<Self> {
                Uuid::parse_str(value).ok().map(Self)
            }

            pub fn to_storage(self) -> String {
                self.0.to_string()
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                let value = self.0.simple().to_string();
                write!(formatter, "{}:{}", $prefix, &value[..8])
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::parse(&value).ok_or_else(|| {
                    serde::de::Error::custom(concat!("not a ", stringify!($name)))
                })
            }
        }

        #[cfg(feature = "schema")]
        impl schemars::JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($name))
            }

            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({
                    "type": "string",
                    "format": "uuid",
                    "description": concat!(
                        stringify!($name),
                        " — a client-minted UUID v4, never derived from content or location."
                    )
                })
            }
        }
    };
}

id_type!(
    /// One locally authoritative chat.
    ChatId,
    "chat"
);
id_type!(
    /// One user or assistant message.
    MessageId,
    "msg"
);
id_type!(
    /// One immutable rendered/searchable message block.
    MessageBlockId,
    "block"
);
id_type!(
    /// One provider invocation, including failure or cancellation.
    InferenceAttemptId,
    "attempt"
);
id_type!(
    /// One append-only response-rating event.
    FeedbackEventId,
    "feedback"
);
id_type!(
    /// One retry-safe Store-as-document operation.
    DocumentExportId,
    "export"
);
id_type!(
    /// One durable, content-free workspace-tool invocation record.
    ToolInvocationId,
    "tool"
);
id_type!(
    /// One picture or file stored with a chat, minted by the client that
    /// adds it so a repeated upload names the same attachment.
    ChatAttachmentId,
    "attachment"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ai_identities_are_uuid_v4_values() {
        for id in [
            ChatId::new().as_uuid(),
            MessageId::new().as_uuid(),
            MessageBlockId::new().as_uuid(),
            InferenceAttemptId::new().as_uuid(),
            FeedbackEventId::new().as_uuid(),
            DocumentExportId::new().as_uuid(),
            ToolInvocationId::new().as_uuid(),
            ChatAttachmentId::new().as_uuid(),
        ] {
            assert_eq!(id.get_version(), Some(uuid::Version::Random));
        }
    }

    #[test]
    fn storage_form_is_lowercase_hyphenated_and_round_trips() {
        let id = ChatId::new();
        let stored = id.to_storage();
        assert_eq!(stored.len(), 36);
        assert_eq!(ChatId::parse(&stored), Some(id));
        assert_eq!(
            serde_json::from_str::<ChatId>(&format!("\"{stored}\"")).unwrap(),
            id
        );
    }

    #[test]
    fn malformed_identity_is_refused() {
        assert!(ChatId::parse("not-a-uuid").is_none());
        assert!(serde_json::from_str::<ChatId>("\"not-a-uuid\"").is_err());
    }
}
