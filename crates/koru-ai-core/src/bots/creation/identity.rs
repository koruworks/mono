// SPDX-License-Identifier: MPL-2.0
//! Bounded authority identity projection, deliberately not configuration validation.
use super::*;
use serde::de::{IgnoredAny, IntoDeserializer, Visitor};
use std::{fmt, marker::PhantomData};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotCreationIdentity {
    pub version: u32,
    pub bot: BotId,
    pub owner: BotOwner,
    pub home: WorkspaceId,
    pub main_chat: ChatId,
}

// Check the identity string bound before handing it to the existing parser.
// Store IDs keep their existing domains; delegate IDs retain their stricter one.
fn identity<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    struct Id<T>(PhantomData<T>);
    impl<'de, T: serde::de::DeserializeOwned> Visitor<'de> for Id<T> {
        type Value = T;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a 36-byte typed identity")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<T, E> {
            if value.len() != 36 {
                return Err(E::custom("identity length"));
            }
            T::deserialize(value.into_deserializer())
        }
    }
    deserializer.deserialize_str(Id(PhantomData))
}

// Struct projections avoid internally-tagged enum buffering of ignored content.
// Unknown nonidentity members are skipped with serde's IgnoredAny traversal;
// no model, settings, folder or role payload is materialized or validated here.
#[derive(Deserialize)]
struct Legacy {
    #[serde(deserialize_with = "identity")]
    id: BotId,
    #[serde(deserialize_with = "identity")]
    user_scope: UserScopeId,
    #[serde(deserialize_with = "identity")]
    owner_device: DeviceId,
    #[serde(deserialize_with = "identity")]
    home_workspace: WorkspaceId,
    #[serde(deserialize_with = "identity")]
    main_chat: ChatId,
    revision: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Configuration,
    CreationV2,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RoleKind {
    Existing,
    Authored,
}
#[derive(Deserialize)]
struct Role {
    kind: RoleKind,
    seed: Option<IgnoredAny>,
}
#[derive(Deserialize)]
struct Creation {
    #[serde(deserialize_with = "identity")]
    bot: BotId,
    #[serde(deserialize_with = "identity")]
    user_scope: UserScopeId,
    #[serde(deserialize_with = "identity")]
    owner_device: DeviceId,
    #[serde(deserialize_with = "identity")]
    home_workspace: WorkspaceId,
    #[serde(deserialize_with = "identity")]
    main_chat: ChatId,
    #[serde(rename = "profile", deserialize_with = "identity")]
    _profile: ProfileId,
    phase: BotCreationPhase,
    role: Role,
    reason: Option<IgnoredAny>,
    #[serde(default)]
    abandoned: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Outcome {
    kind: Kind,
    version: u32,
    #[serde(default, deserialize_with = "present")]
    bot: Option<Legacy>,
    #[serde(default, deserialize_with = "present")]
    creation: Option<Creation>,
}
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// Only ownership/correlation and completed creation readiness are projected.
/// Existing boot additionally performs full receipt/configuration validation.
/// The repository must independently require the outer completed create row.
pub fn bot_creation_identity(json: &str) -> Result<BotCreationIdentity, BotError> {
    if json.len() > BOT_RECEIPT_OUTCOME_LIMIT {
        return Err(BotError::Storage);
    }
    let value: Outcome = serde_json::from_str(json).map_err(|_| BotError::Storage)?;
    let version = value.version;
    let (bot, scope, device, home, main_chat) = match value {
        Outcome {
            kind: Kind::Configuration,
            version: 1,
            bot: Some(b),
            creation: None,
        } if b.revision == 1 => (
            b.id,
            b.user_scope,
            b.owner_device,
            b.home_workspace,
            b.main_chat,
        ),
        Outcome {
            kind: Kind::CreationV2,
            version: 2,
            creation: Some(c),
            bot: None,
        } if !c.abandoned
            && c.reason.is_none()
            && c.role.seed.is_none()
            && matches!(
                (c.role.kind, c.phase),
                (RoleKind::Existing, BotCreationPhase::FolderReady)
                    | (RoleKind::Authored, BotCreationPhase::RoleVerified)
            ) =>
        {
            (
                c.bot,
                c.user_scope,
                c.owner_device,
                c.home_workspace,
                c.main_chat,
            )
        }
        _ => return Err(BotError::Storage),
    };
    Ok(BotCreationIdentity {
        version,
        bot,
        owner: BotOwner {
            user_scope: scope,
            device,
        },
        home,
        main_chat,
    })
}

#[cfg(test)]
mod tests;
