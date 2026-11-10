// SPDX-License-Identifier: MPL-2.0

//! Plan 13 strict issuer DTOs. Routing metadata remains candidate evidence.
//! Shape is not authority: clients must verify the token and correlate every
//! dataset, expiry and route with their current trusted operation context.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use koru_account_core::ProfileId;
use koru_device::cloud::CloudReplicaPrincipalV1;
use serde::{Deserialize, Deserializer, Serialize, de};
use uuid::{Uuid, Variant, Version};

use crate::{CloudSyncRouteV1, Version1};

pub const MAX_SYNC_CREDENTIAL_BYTES: usize = 10_240;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncTokenRequestV1 {
    pub version: Version1,
    #[serde(deserialize_with = "profile_id")]
    pub profile_id: ProfileId,
}

fn profile_id<'de, D: Deserializer<'de>>(d: D) -> Result<ProfileId, D::Error> {
    let value = String::deserialize(d)?;
    let id = Uuid::parse_str(&value).map_err(de::Error::custom)?;
    if id.to_string() != value
        || id.get_version() != Some(Version::Random)
        || id.get_variant() != Variant::RFC4122
    {
        return Err(de::Error::custom("invalid canonical v4 Profile UUID"));
    }
    Ok(ProfileId::from_uuid(id))
}

/// Bounded compact evidence, not a verified token. Debug redacts its bytes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SyncCredential(String);

impl TryFrom<String> for SyncCredential {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.len() > MAX_SYNC_CREDENTIAL_BYTES || !value.is_ascii() {
            return Err("invalid compact credential bound");
        }
        let parts: Vec<_> = value.split('.').collect();
        if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
            return Err("invalid compact credential shape");
        }
        for (index, part) in parts.iter().enumerate() {
            let bytes = URL_SAFE_NO_PAD
                .decode(part)
                .map_err(|_| "invalid compact encoding")?;
            if index == 2 && bytes.len() != 64 {
                return Err("invalid compact signature length");
            }
        }
        Ok(Self(value))
    }
}

impl From<SyncCredential> for String {
    fn from(value: SyncCredential) -> Self {
        value.0
    }
}

impl SyncCredential {
    pub fn compact(&self) -> &str {
        &self.0
    }

    /// The credential's `workspace_id` claim, read **unverified**: the
    /// Workspace a Profile session is scoped to on the worker, whose frames
    /// must name it. Addressing only, never authority: the worker verifies
    /// the credential, so a wrong value here is a refusal there. `None` when
    /// the payload carries no such claim.
    pub fn unverified_workspace_id(&self) -> Option<Uuid> {
        #[derive(Deserialize)]
        struct Claim {
            workspace_id: Uuid,
        }
        let payload = self.0.split('.').nth(1)?;
        let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
        serde_json::from_slice::<Claim>(&bytes)
            .ok()
            .map(|claim| claim.workspace_id)
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;

    fn compact(body: &str) -> SyncCredential {
        let head = URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA"}"#);
        let signature = URL_SAFE_NO_PAD.encode([9_u8; 64]);
        SyncCredential::try_from(format!(
            "{head}.{}.{signature}",
            URL_SAFE_NO_PAD.encode(body)
        ))
        .unwrap()
    }

    #[test]
    fn the_workspace_claim_is_read_for_addressing_and_absent_when_missing() {
        let workspace = Uuid::from_u128(0x2000_0000_0000_4000_8000_0000_0000_0001);
        let issued = compact(&format!(
            r#"{{"ver":1,"sub":"x","workspace_id":"{workspace}","profile_id":"y"}}"#
        ));
        assert_eq!(issued.unverified_workspace_id(), Some(workspace));
        assert_eq!(compact(r#"{"sub":"x"}"#).unverified_workspace_id(), None);
        assert_eq!(
            compact(r#"{"workspace_id":"not-a-uuid"}"#).unverified_workspace_id(),
            None
        );
    }
}

impl std::fmt::Debug for SyncCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyncCredential(<redacted>)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DatasetRouteResultV1 {
    Available {
        dataset: CloudReplicaPrincipalV1,
        route: CloudSyncRouteV1,
    },
    Unavailable {
        dataset: CloudReplicaPrincipalV1,
        code: SyncUnavailableCode,
        #[serde(deserialize_with = "retry_seconds")]
        retry_after_seconds: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncUnavailableCode {
    #[serde(rename = "sync_unavailable")]
    SyncUnavailable,
}

fn retry_seconds<'de, D: Deserializer<'de>>(d: D) -> Result<u16, D::Error> {
    let value = u16::deserialize(d)?;
    if !(1..=300).contains(&value) {
        return Err(de::Error::custom("invalid retry interval"));
    }
    Ok(value)
}

impl DatasetRouteResultV1 {
    pub fn dataset(&self) -> CloudReplicaPrincipalV1 {
        match self {
            Self::Available { dataset, .. } | Self::Unavailable { dataset, .. } => *dataset,
        }
    }
    pub fn available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
    fn valid_retry(&self) -> bool {
        match self {
            Self::Available { .. } => true,
            Self::Unavailable {
                retry_after_seconds,
                ..
            } => (1..=300).contains(retry_after_seconds),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ResponseFields", deny_unknown_fields)]
pub struct SyncTokenResponseV1 {
    version: Version1,
    credential: SyncCredential,
    expires_at_seconds: i64,
    workspace: DatasetRouteResultV1,
    profile: DatasetRouteResultV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseFields {
    version: Version1,
    credential: SyncCredential,
    expires_at_seconds: i64,
    workspace: DatasetRouteResultV1,
    profile: DatasetRouteResultV1,
}

impl TryFrom<ResponseFields> for SyncTokenResponseV1 {
    type Error = &'static str;
    fn try_from(v: ResponseFields) -> Result<Self, Self::Error> {
        let _ = v.version;
        Self::new(v.credential, v.expires_at_seconds, v.workspace, v.profile)
    }
}

impl SyncTokenResponseV1 {
    pub fn new(
        credential: SyncCredential,
        expires_at_seconds: i64,
        workspace: DatasetRouteResultV1,
        profile: DatasetRouteResultV1,
    ) -> Result<Self, &'static str> {
        if expires_at_seconds < 0
            || !matches!(
                workspace.dataset(),
                CloudReplicaPrincipalV1::Workspace { .. }
            )
            || !matches!(profile.dataset(), CloudReplicaPrincipalV1::Profile { .. })
            || (!workspace.available() && !profile.available())
            || !workspace.valid_retry()
            || !profile.valid_retry()
        {
            return Err("invalid sync token response");
        }
        Ok(Self {
            version: Version1,
            credential,
            expires_at_seconds,
            workspace,
            profile,
        })
    }
    pub fn credential(&self) -> &SyncCredential {
        &self.credential
    }
    pub fn expires_at_seconds(&self) -> i64 {
        self.expires_at_seconds
    }
    pub fn workspace(&self) -> &DatasetRouteResultV1 {
        &self.workspace
    }
    pub fn profile(&self) -> &DatasetRouteResultV1 {
        &self.profile
    }
}

impl schemars::JsonSchema for SyncTokenRequestV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SyncTokenRequestV1".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type":"object","additionalProperties":false,
            "required":["version","profile_id"],"properties":{
                "version":{"type":"integer","const":1},
                "profile_id":{"type":"string","format":"uuid",
                    "pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"}}})
    }
}

fn dataset_route_schema(workspace: bool, route: schemars::Schema) -> serde_json::Value {
    let (kind, authority, lineage) = if workspace {
        ("workspace", "workspace_id", "incarnation_id")
    } else {
        ("profile", "profile_id", "lineage_id")
    };
    let uuid = serde_json::json!({"type":"string","format":"uuid",
        "pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"});
    let mut properties = serde_json::Map::new();
    properties.insert(
        "kind".into(),
        serde_json::json!({"type":"string","const":kind}),
    );
    for name in [authority, lineage, "logical_replica_id"] {
        properties.insert(name.into(), uuid.clone());
    }
    let dataset = serde_json::json!({"type":"object","additionalProperties":false,
        "required":["kind",authority,lineage,"logical_replica_id"],"properties":properties});
    serde_json::json!({"oneOf":[
        {"type":"object","additionalProperties":false,"required":["state","dataset","route"],
            "properties":{"state":{"type":"string","const":"available"},"dataset":dataset,"route":route}},
        {"type":"object","additionalProperties":false,"required":["state","dataset","code","retry_after_seconds"],
            "properties":{"state":{"type":"string","const":"unavailable"},"dataset":dataset,
                "code":{"type":"string","const":"sync_unavailable"},
                "retry_after_seconds":{"type":"integer","minimum":1,"maximum":300}}}
    ]})
}

impl schemars::JsonSchema for SyncTokenResponseV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SyncTokenResponseV1".into()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let route = generator.subschema_for::<CloudSyncRouteV1>();
        schemars::json_schema!({"type":"object","additionalProperties":false,
            "required":["version","credential","expires_at_seconds","workspace","profile"],
            "properties":{
                "version":{"type":"integer","const":1},
                "credential":{"type":"string","minLength":1,"maxLength":MAX_SYNC_CREDENTIAL_BYTES},
                "expires_at_seconds":{"type":"integer","minimum":0,"maximum":i64::MAX},
                "workspace":dataset_route_schema(true,route.clone()),
                "profile":dataset_route_schema(false,route)
            },
            "anyOf":[{"properties":{"workspace":{"properties":{"state":{"const":"available"}}}}},
                {"properties":{"profile":{"properties":{"state":{"const":"available"}}}}}]
        })
    }
}

#[cfg(test)]
mod tests;
