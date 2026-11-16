// SPDX-License-Identifier: MPL-2.0

//! Size-bounded transport only. The account service and surface validate the
//! canonical exact-source DTO; this serde-only leaf does not own its schema.
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

macro_rules! payload {
    ($name:ident, $limit:expr) => {
        #[derive(Clone, PartialEq, Eq)]
        pub struct $name(String);
        impl $name {
            pub fn new(json: String) -> Result<Self, &'static str> {
                if json.is_empty() || json.len() > $limit {
                    return Err("invalid Cloud document payload size");
                }
                Ok(Self(json))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(d)?).map_err(de::Error::custom)
            }
        }
    };
}
payload!(CloudDocumentSelectionPayload, 1024);
payload!(CloudDocumentInspectionPayload, 1024 * 1024);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudInspectionFailure {
    Unavailable,
    SourceChanged,
    OutcomeUnknown,
}
impl CloudInspectionFailure {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "cloud_document_unavailable",
            Self::SourceChanged => "cloud_document_source_changed",
            Self::OutcomeUnknown => "cloud_edit_outcome_unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cloud_document_transport_is_bounded_and_redacted_before_queueing() {
        let mut command = serde_json::json!({"kind":"inspect_cloud_document","request_id":3,"selection":"private-selection"});
        let parsed: crate::AccountCommand = serde_json::from_value(command.clone()).unwrap();
        assert!(!format!("{parsed:?}").contains("private-selection"));
        command["selection"] = serde_json::json!("x".repeat(1025));
        assert!(serde_json::from_value::<crate::AccountCommand>(command).is_err());
        assert!(CloudDocumentInspectionPayload::new("x".repeat(1024 * 1024 + 1)).is_err());
        let outcome = crate::AccountOperationResult::CloudFileRemoval {
            request_id: 3,
            selection: CloudDocumentSelectionPayload::new("private-selection".into()).unwrap(),
            node_id: "private-node".into(),
            removed: false,
            failure: Some(CloudInspectionFailure::OutcomeUnknown),
        };
        assert!(!format!("{outcome:?}").contains("private"));
        assert_eq!(
            serde_json::from_str::<crate::AccountOperationResult>(
                &serde_json::to_string(&outcome).unwrap()
            )
            .unwrap(),
            outcome
        );
    }
}
