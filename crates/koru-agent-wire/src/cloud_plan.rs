// SPDX-License-Identifier: MPL-2.0

//! Bounded transport of the already versioned account HTTP DTO. This leaf does
//! not own its policy fields: the account worker and surface validate that DTO.
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

pub const MAX_CLOUD_PLAN_BYTES: usize = 32 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct CloudPlanPayload(String);
impl CloudPlanPayload {
    pub fn new(json: String) -> Result<Self, &'static str> {
        if json.is_empty() || json.len() > MAX_CLOUD_PLAN_BYTES {
            return Err("invalid Cloud plan payload size");
        }
        Ok(Self(json))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for CloudPlanPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CloudPlanPayload(<redacted>)")
    }
}
impl Serialize for CloudPlanPayload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for CloudPlanPayload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plan_transport_is_bounded_redacted_and_not_a_policy_decoder() {
        let payload = CloudPlanPayload::new("{\"private\":1}".into()).unwrap();
        assert_eq!(
            serde_json::from_str::<CloudPlanPayload>(&serde_json::to_string(&payload).unwrap())
                .unwrap(),
            payload
        );
        assert!(!format!("{payload:?}").contains("private"));
        assert!(CloudPlanPayload::new(String::new()).is_err());
        assert!(
            serde_json::from_str::<CloudPlanPayload>(
                &serde_json::to_string(&"x".repeat(MAX_CLOUD_PLAN_BYTES + 1)).unwrap()
            )
            .is_err()
        );
    }
}
