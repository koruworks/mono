// SPDX-License-Identifier: MPL-2.0

//! Stable identifiers.
//!
//! Every id is a **client-minted v4 UUID**, never derived from a path, a folder,
//! a machine name, a locale or anything a person can translate. That is ADR-39
//! rule 1, and the internationalization invariants extend it for a reason worth
//! stating: names are translatable and identities are not, so the moment an id
//! carries a name it stops being stable in another language.
//!
//! **This macro is a deliberate copy of `koru-core`'s `id_type!`.** The obvious
//! move is to depend on `koru-core` and reuse it. That crate is the
//! one-giant-`LoroDoc` model this store replaces, so depending on it to reach
//! sixty lines of newtype would import the whole shape we are redoing. The
//! duplication is the cheaper of the two, and this comment is the written reason
//! `PRINCIPLES.md` asks for.

use std::fmt;

use uuid::Uuid;

/// Declares a newtype over `Uuid` with the usual conversions.
macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }

            /// The form stored in SQLite, in Loro, in `.koru` bundles and in
            /// URLs. One spelling everywhere: a hyphenated lowercase UUID.
            pub fn to_storage(&self) -> String {
                self.0.to_string()
            }

            pub fn parse(s: &str) -> Option<Self> {
                Uuid::parse_str(s).ok().map(Self)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        /// Prefixed and truncated — ids appear in test failures constantly, and
        /// a full UUID drowns the message.
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let s = self.0.simple().to_string();
                write!(f, "{}:{}", $prefix, &s[..8])
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.to_storage())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::parse(&s).ok_or_else(|| {
                    serde::de::Error::custom(concat!("not a ", stringify!($name)))
                })
            }
        }

        /// **Written by hand because the serde above is.** `#[derive(JsonSchema)]`
        /// reads a type's *fields*; this one serializes as a string through an
        /// impl of its own, so a derive would describe the `Uuid` inside rather
        /// than the string that actually crosses.
        ///
        /// Behind the `schema` feature — see the crate manifest for why the
        /// impl cannot live in `koru-store-wire` instead.
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
                        " — a client-minted v4 UUID, hyphenated and lowercase. \
                         Immutable, and never derived from a name or a path."
                    ),
                })
            }
        }
    };
}

id_type!(
    /// A profile — a persona, not a folder. "Steven — Work", "Steven — Personal".
    /// Created entirely offline; an account may be attached later and is not
    /// this id ([`crate::model::Profile::account`]).
    ProfileId,
    "prof"
);
id_type!(
    /// A workspace: the unit of export, backup and (later) sharing.
    ///
    /// Globally unique and **immutable**. The same logical workspace may exist
    /// as separate physical replicas in several channels and on several
    /// machines; they share this id and never share a database file.
    WorkspaceId,
    "ws"
);
id_type!(
    /// One Loro document inside a workspace — including the manifest, which is
    /// a document like any other so that the persistence layer has one case
    /// rather than two.
    DocumentId,
    "doc"
);
id_type!(
    /// One coordinated attempt to remove a Profile on this installation.
    RemovalAttemptId,
    "removal"
);
id_type!(
    /// An API client holding a scoped credential. Not a person and not a device.
    ClientId,
    "client"
);
id_type!(
    /// Immutable state captured for a document. Several history entries may
    /// name one version when a boundary is recorded without a content change.
    DocumentVersionId,
    "ver"
);
id_type!(
    /// One user-visible point on a document's history timeline.
    HistoryEntryId,
    "hist"
);
id_type!(
    /// The local validated user binding a root catalog and its personal units
    /// are keyed by (`docs/features/root-catalog/TECHNICAL.final.md` §4.1).
    ///
    /// Before sign-in one device-local handle exists, kept in `store_state`
    /// under [`K_LOCAL_USER_SCOPE`](crate::root_catalog::K_LOCAL_USER_SCOPE).
    /// It is a SQL row key and an authorization boundary, **never a field of
    /// any Loro unit**: it cannot be dropped merely because the UUIDs it
    /// scopes are globally unique, and it cannot be claimed by a document.
    UserScopeId,
    "scope"
);
id_type!(
    /// One deletion fact in a root-catalog unit: the key of
    /// `deletions[DeleteId]` and of the `purged[DeleteId]` evidence that may
    /// follow it. Minted per deletion, so two deletions of one subject are
    /// two facts and a restore can name exactly the ones it observed.
    DeletionId,
    "del"
);
id_type!(
    /// One restore fact in a root-catalog unit: the key of
    /// `restores[RestoreId]`.
    RestoreId,
    "restore"
);
id_type!(
    /// One Profile-deletion operation: the opaque `batch_id` every deletion
    /// fact it cascaded carries, and the key of its local application job
    /// (`docs/features/root-catalog/TECHNICAL.final.md` §4.2, §6.1). Never a
    /// Profile or Workspace id. Stored in the unit as a bare UUID.
    BatchId,
    "batch"
);

/// Reminder identity is new storage, so it starts with the repository's full
/// canonical contract: version 4 and lowercase hyphenated spelling on read as
/// well as write. Established Store IDs retain their historical permissive
/// reader above; tightening those would be a migration, not reminder work.
macro_rules! strict_v4_id_type {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self { Self(Uuid::new_v4()) }

            pub fn parse(value: &str) -> Option<Self> {
                let id = Uuid::parse_str(value).ok()?;
                (id.get_version() == Some(uuid::Version::Random) && id.to_string() == value)
                    .then_some(Self(id))
            }

            pub fn as_uuid(self) -> Uuid { self.0 }
            pub fn to_storage(self) -> String { self.0.to_string() }
        }

        impl Default for $name {
            fn default() -> Self { Self::new() }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}:{}", $prefix, &self.0.simple().to_string()[..8])
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}", self.0) }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0.to_string())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::parse(&value).ok_or_else(|| {
                    serde::de::Error::custom(concat!("not a canonical v4 ", stringify!($name)))
                })
            }
        }

        #[cfg(feature = "schema")]
        impl schemars::JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($name))
            }
            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({"type":"string", "format":"uuid"})
            }
        }
    };
}

strict_v4_id_type!(
    /// One reminder rule identity.
    ReminderId,
    "rem"
);
strict_v4_id_type!(
    /// One immutable whole-rule revision.
    RuleRevisionId,
    "rrev"
);
strict_v4_id_type!(
    /// One append-only occurrence action.
    ActionId,
    "ract"
);

#[cfg(test)]
mod tests;
