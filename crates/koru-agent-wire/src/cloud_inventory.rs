// SPDX-License-Identifier: MPL-2.0

//! Bounded transport of the already versioned account HTTP DTO. This leaf does
//! not own its policy fields: the account worker and surface validate that DTO.
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

pub const MAX_CLOUD_INVENTORY_BYTES: usize = 1024 * 1024;

pub(crate) fn deserialize_cursor<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    let cursor = Option::<String>::deserialize(d)?;
    if cursor.as_ref().is_some_and(|s| {
        s.is_empty()
            || s.len() > 2048
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }) {
        return Err(de::Error::custom("invalid inventory cursor"));
    }
    Ok(cursor)
}

pub(crate) fn deserialize_id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let id = String::deserialize(d)?;
    if id.len() != 36
        || !id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
    {
        return Err(de::Error::custom("invalid inventory identity"));
    }
    Ok(id)
}

#[derive(Clone, PartialEq, Eq)]
pub struct CloudInventoryPayload(String);
impl CloudInventoryPayload {
    pub fn new(json: String) -> Result<Self, &'static str> {
        if json.is_empty() || json.len() > MAX_CLOUD_INVENTORY_BYTES {
            return Err("invalid Cloud inventory payload size");
        }
        Ok(Self(json))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for CloudInventoryPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CloudInventoryPayload(<redacted>)")
    }
}
impl Serialize for CloudInventoryPayload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for CloudInventoryPayload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selectors_are_bounded_before_desktop_queue_admission() {
        let mut command = serde_json::json!({
            "kind":"inspect_cloud_usage_documents", "request_id":42,
            "workspace_id":"00000000-0000-0000-0000-000000000050",
            "incarnation_id":"00000000-0000-0000-0000-000000000051", "cursor":null
        });
        let parsed: crate::AccountCommand = serde_json::from_value(command.clone()).unwrap();
        assert!(!format!("{parsed:?}").contains("00000000"));
        command["cursor"] = "x".repeat(2049).into();
        assert!(serde_json::from_value::<crate::AccountCommand>(command.clone()).is_err());
        command["cursor"] = "a/b".into();
        assert!(serde_json::from_value::<crate::AccountCommand>(command.clone()).is_err());
        command["cursor"] = serde_json::Value::Null;
        command["workspace_id"] = "x".repeat(10000).into();
        assert!(serde_json::from_value::<crate::AccountCommand>(command).is_err());
    }
    #[test]
    fn inventory_transport_is_bounded_redacted_and_not_a_policy_decoder() {
        let payload = CloudInventoryPayload::new("{\"private\":1}".into()).unwrap();
        assert_eq!(
            serde_json::from_str::<CloudInventoryPayload>(
                &serde_json::to_string(&payload).unwrap()
            )
            .unwrap(),
            payload
        );
        assert!(!format!("{payload:?}").contains("private"));
        assert!(CloudInventoryPayload::new(String::new()).is_err());
        assert!(
            serde_json::from_str::<CloudInventoryPayload>(
                &serde_json::to_string(&"x".repeat(MAX_CLOUD_INVENTORY_BYTES + 1)).unwrap()
            )
            .is_err()
        );
    }
}

/// Stable read failure, never a numeric empty inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudInventoryFailure {
    RevisionChanged,
    Unavailable,
}
impl CloudInventoryFailure {
    pub fn code(self) -> &'static str {
        match self {
            Self::RevisionChanged => "usage_revision_changed",
            Self::Unavailable => "cloud_inventory_unavailable",
        }
    }
}
