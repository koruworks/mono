// SPDX-License-Identifier: MPL-2.0

//! Read-only current durable-unit inventory. Membership is not cleanup authority,
//! and per-unit file amounts overlap; they are never an account total.
use super::*;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use std::borrow::Cow;

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudUsageDocumentsQueryV1 {
    #[schemars(
        with = "String",
        regex(pattern = "^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$")
    )]
    pub incarnation_id: ReadUuid,
    #[schemars(range(min = 1, max = 100))]
    pub limit: Option<u16>,
    #[schemars(length(min = 1, max = 2048), regex(pattern = "^[A-Za-z0-9_-]+$"))]
    pub cursor: Option<String>,
}
impl CloudUsageDocumentsQueryV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        CloudUsageWorkspacesQueryV1 {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
        .validate()
    }
}
impl std::fmt::Debug for CloudUsageDocumentsQueryV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CloudUsageDocumentsQueryV1(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentsContract {
    #[serde(rename = "cloud-usage-documents-v1")]
    V1,
}
vocabulary!(RowTotals { NonAdditive });
vocabulary!(UnitRole {
    Content,
    Structure,
    Unknown
});
vocabulary!(ManifestState { Verified, Unknown });
vocabulary!(ManifestReason {
    ManifestUnavailable,
    CloudSchemaUpdateRequired
});
vocabulary!(MembershipState {
    Listed,
    Trash,
    Unfiled,
    Structural,
    Unknown
});
vocabulary!(MembershipReason {
    ManifestUnavailable,
    CloudSchemaUpdateRequired,
    UnitIdentityUnknown
});

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

object!(DocumentManifestV1 {
    state: ManifestState,
    reason: Option<ManifestReason>,
    source_head_digest: Option<String>,
    proof_digest: Option<String>
});
impl Validate for DocumentManifestV1 {
    fn validate(&self) -> Result<(), &'static str> {
        if self.source_head_digest.as_ref().is_some_and(|s| !digest(s))
            || self.proof_digest.as_ref().is_some_and(|s| !digest(s))
        {
            return Err("invalid manifest digest");
        }
        let valid = match self.state {
            ManifestState::Verified => {
                self.reason.is_none()
                    && self.source_head_digest.is_some()
                    && self.proof_digest.is_some()
            }
            ManifestState::Unknown => self.reason.is_some() && self.proof_digest.is_none(),
        };
        if valid {
            Ok(())
        } else {
            Err("invalid manifest proof state")
        }
    }
}
object!(DocumentMembershipV1 { state: MembershipState, reason: Option<MembershipReason> });
impl Validate for DocumentMembershipV1 {
    fn validate(&self) -> Result<(), &'static str> {
        if (self.state == MembershipState::Unknown) == self.reason.is_some() {
            Ok(())
        } else {
            Err("invalid membership state")
        }
    }
}
object!(DocumentUsageV1 { state: UsageState, reason: Option<UsageReason>, files_bytes: Option<Decimal> });
impl Validate for DocumentUsageV1 {
    fn validate(&self) -> Result<(), &'static str> {
        let valid = match self.state {
            UsageState::Known => self.reason.is_none() && self.files_bytes.is_some(),
            UsageState::Unknown => self.reason.is_some() && self.files_bytes.is_none(),
            UsageState::Building => self.reason.is_none() && self.files_bytes.is_none(),
        };
        if valid {
            Ok(())
        } else {
            Err("invalid document usage state")
        }
    }
}
object!(DocumentUsageItemV1 {
    cloud_unit_id: ReadUuid,
    document_id: Option<ReadUuid>,
    unit_role: UnitRole,
    source_head_digest: Option<String>,
    display_name: Option<String>,
    membership: DocumentMembershipV1,
    usage: DocumentUsageV1
});
impl Validate for DocumentUsageItemV1 {
    fn validate(&self) -> Result<(), &'static str> {
        if self.source_head_digest.as_ref().is_some_and(|s| !digest(s))
            || self
                .display_name
                .as_ref()
                .is_some_and(|s| s.chars().count() > 128)
            || (self.usage.state == UsageState::Known && self.source_head_digest.is_none())
        {
            return Err("invalid document source or label");
        }
        if !matches!(
            self.membership.state,
            MembershipState::Listed | MembershipState::Trash
        ) && self.display_name.is_some()
        {
            return Err("unproven document label");
        }
        let valid = match self.unit_role {
            UnitRole::Unknown => {
                self.document_id.is_none()
                    && self.membership.state == MembershipState::Unknown
                    && self.membership.reason == Some(MembershipReason::UnitIdentityUnknown)
                    && self.usage.state == UsageState::Unknown
            }
            UnitRole::Content => {
                self.document_id.is_some() && self.membership.state != MembershipState::Structural
            }
            UnitRole::Structure => {
                self.document_id.is_some()
                    && self.membership.state == MembershipState::Structural
                    && (self.usage.state != UsageState::Known
                        || self.usage.files_bytes == Some(Decimal(0)))
            }
        };
        if valid {
            Ok(())
        } else {
            Err("inconsistent unit identity")
        }
    }
}
object!(CloudUsageDocumentsResponseV1 {
    contract: DocumentsContract,
    algorithm: FilesAlgorithm,
    row_totals: RowTotals,
    revision: Decimal,
    generation: Option<ReadUuid>,
    evaluated_at_millis: Decimal,
    workspace_id: ReadUuid,
    incarnation_id: ReadUuid,
    source_revision: Decimal,
    inventory_digest: String,
    manifest: DocumentManifestV1,
    items: Vec<DocumentUsageItemV1>,
    next_cursor: Option<String>
});
impl Validate for CloudUsageDocumentsResponseV1 {
    fn validate(&self) -> Result<(), &'static str> {
        if !digest(&self.inventory_digest) || self.items.len() > 100 {
            return Err("invalid inventory page");
        }
        CloudUsageWorkspacesQueryV1 {
            limit: None,
            cursor: self.next_cursor.clone(),
        }
        .validate()?;
        if self
            .items
            .windows(2)
            .any(|w| w[0].cloud_unit_id.0 >= w[1].cloud_unit_id.0)
        {
            return Err("unordered or duplicate unit");
        }
        for item in &self.items {
            if (self.generation.is_none() && item.usage.state == UsageState::Known)
                || (self.generation.is_some() && item.usage.state == UsageState::Building)
            {
                return Err("usage generation mismatch");
            }
            if self.manifest.state != ManifestState::Verified
                && !matches!(
                    item.membership.state,
                    MembershipState::Unknown | MembershipState::Structural
                )
            {
                return Err("membership lacks a current manifest proof");
            }
        }
        Ok(())
    }
}
impl JsonSchema for CloudUsageDocumentsResponseV1 {
    fn schema_name() -> Cow<'static, str> {
        "CloudUsageDocumentsResponseV1".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        let source: serde_json::Value =
            serde_json::from_str(include_str!("../cloud_documents.schema.json"))
                .expect("reviewed inventory schema");
        super::schema::expand(&source["$defs"]["page"], &source["$defs"])
            .try_into()
            .expect("reviewed inventory schema object")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn fixtures() -> Value {
        serde_json::from_str(include_str!("../cloud_documents.fixtures.json")).unwrap()
    }
    fn page() -> Value {
        fixtures()["fixtures"][0]["response"].clone()
    }
    #[test]
    fn all_reviewed_inventory_fixtures_roundtrip_exactly() {
        let all = fixtures();
        assert_eq!(all["fixtures"].as_array().unwrap().len(), 20);
        for fixture in all["fixtures"].as_array().unwrap() {
            let value = fixture["response"].clone();
            let encoded = if fixture["http_status"] == 200 {
                serde_json::to_value(
                    serde_json::from_value::<CloudUsageDocumentsResponseV1>(value.clone()).unwrap(),
                )
                .unwrap()
            } else {
                serde_json::to_value(
                    serde_json::from_value::<crate::DeveloperErrorV1>(value.clone()).unwrap(),
                )
                .unwrap()
            };
            assert_eq!(value, encoded, "{}", fixture["name"]);
        }
    }
    #[test]
    fn refuses_incomplete_and_inconsistent_inventory_claims() {
        for mutate in [
            |v: &mut Value| {
                v["items"][1]["usage"]["files_bytes"] = json!(9007199254740992u64);
            },
            |v: &mut Value| {
                v["items"][1]["usage"]["files_bytes"] = json!("9223372036854775808");
            },
            |v: &mut Value| {
                v["items"][1]["usage"]["files_bytes"] = json!("01");
            },
            |v: &mut Value| {
                v["items"][1]["source_head_digest"] = Value::Null;
            },
            |v: &mut Value| {
                v["items"][1]["display_name"] = json!("a".repeat(129));
            },
            |v: &mut Value| {
                v["items"][3]["display_name"] = json!("unproven");
            },
            |v: &mut Value| {
                v["items"][0]["usage"]["files_bytes"] = json!("1");
            },
            |v: &mut Value| {
                v["items"][1]["unit_role"] = json!("future");
            },
            |v: &mut Value| {
                v["generation"] = Value::Null;
            },
            |v: &mut Value| {
                v["items"][1]["cloud_unit_id"] = v["items"][0]["cloud_unit_id"].clone();
            },
            |v: &mut Value| {
                v["manifest"] = json!({"state":"unknown","reason":"manifest_unavailable","source_head_digest":null,"proof_digest":null});
            },
            |v: &mut Value| {
                v.as_object_mut().unwrap().remove("next_cursor");
            },
        ] {
            let mut value = page();
            mutate(&mut value);
            assert!(serde_json::from_value::<CloudUsageDocumentsResponseV1>(value).is_err());
        }
        let mut value = page();
        value["items"][1]["display_name"] = json!("😀".repeat(128));
        assert!(serde_json::from_value::<CloudUsageDocumentsResponseV1>(value).is_ok());
    }
    #[test]
    fn inventory_debug_does_not_disclose_owner_content_or_cursor() {
        let response: CloudUsageDocumentsResponseV1 = serde_json::from_value(page()).unwrap();
        assert_eq!(
            format!("{response:?}"),
            "CloudUsageDocumentsResponseV1(<redacted>)"
        );
        let query = CloudUsageDocumentsQueryV1 {
            incarnation_id: response.incarnation_id,
            limit: Some(100),
            cursor: Some("privatecursor".into()),
        };
        assert_eq!(
            format!("{query:?}"),
            "CloudUsageDocumentsQueryV1(<redacted>)"
        );
    }
}
