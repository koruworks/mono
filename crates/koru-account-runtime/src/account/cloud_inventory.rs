// SPDX-License-Identifier: MPL-2.0

//! One authorized page per native request. No local census, policy evaluation,
//! accumulated pages, or content opening is hidden behind these reads.
use super::*;
use koru_account_wire::cloud_plan::documents::CloudUsageDocumentsQueryV1;
use koru_account_wire::cloud_plan::{CloudUsageWorkspacesQueryV1, ReadUuid};
use koru_agent_wire::{CloudInventoryFailure, CloudInventoryPayload};

pub(super) fn validate_cursor(cursor: &Option<String>) -> Result<(), ServiceError> {
    if cursor.as_ref().is_some_and(|s| s.len() > 2048) {
        return Err(ServiceError::terminal("invalid_request"));
    }
    CloudUsageWorkspacesQueryV1 {
        limit: Some(100),
        cursor: cursor.clone(),
    }
    .validate()
    .map_err(|_| ServiceError::terminal("invalid_request"))
}

pub(super) fn read_uuid(value: &str) -> Result<ReadUuid, ServiceError> {
    if value.len() != 36 {
        return Err(ServiceError::terminal("invalid_request"));
    }
    serde_json::from_value(serde_json::Value::String(value.into()))
        .map_err(|_| ServiceError::terminal("invalid_request"))
}

fn payload<T: serde::Serialize>(
    response: Result<T, ClientError>,
    cancel: &Cancel,
) -> (Option<CloudInventoryPayload>, Option<CloudInventoryFailure>) {
    if cancel.cancelled() {
        return (None, Some(CloudInventoryFailure::Unavailable));
    }
    match response {
        Ok(page) => match serde_json::to_string(&page)
            .ok()
            .and_then(|json| CloudInventoryPayload::new(json).ok())
        {
            Some(payload) => (Some(payload), None),
            None => (None, Some(CloudInventoryFailure::Unavailable)),
        },
        Err(ClientError::Api { code, .. }) if code.as_str() == "usage_revision_changed" => {
            (None, Some(CloudInventoryFailure::RevisionChanged))
        }
        Err(_) => (None, Some(CloudInventoryFailure::Unavailable)),
    }
}

impl ProductionAccountService {
    pub(super) fn workspace_inventory(
        &self,
        generation: u64,
        request_id: u64,
        cursor: Option<String>,
        cancel: &Cancel,
    ) -> Result<ServiceOutcome, ServiceError> {
        validate_cursor(&cursor)?;
        let response = if cancel.cancelled() {
            Err(ClientError::InvalidRequest)
        } else {
            self.client.cloud_usage_workspaces(
                &self.authorization()?,
                &CloudUsageWorkspacesQueryV1 {
                    limit: Some(100),
                    cursor,
                },
            )
        };
        let (payload, failure) = payload(response, cancel);
        Ok(ServiceOutcome {
            state: self.state(generation, AccountStage::SignedIn),
            credential: CredentialChange::Keep,
            result: AccountOperationResult::CloudUsageWorkspaces {
                request_id,
                payload,
                failure,
            },
            follow_up: None,
        })
    }

    pub(super) fn document_inventory(
        &self,
        generation: u64,
        request_id: u64,
        workspace_id: String,
        incarnation_id: String,
        cursor: Option<String>,
        cancel: &Cancel,
    ) -> Result<ServiceOutcome, ServiceError> {
        validate_cursor(&cursor)?;
        let workspace = read_uuid(&workspace_id)?;
        let incarnation = read_uuid(&incarnation_id)?;
        let response = if cancel.cancelled() {
            Err(ClientError::InvalidRequest)
        } else {
            self.client.cloud_usage_documents(
                &self.authorization()?,
                workspace,
                &CloudUsageDocumentsQueryV1 {
                    incarnation_id: incarnation,
                    limit: Some(100),
                    cursor,
                },
            )
        };
        let (payload, failure) = payload(response, cancel);
        Ok(ServiceOutcome {
            state: self.state(generation, AccountStage::SignedIn),
            credential: CredentialChange::Keep,
            result: AccountOperationResult::CloudUsageDocuments {
                request_id,
                workspace_id,
                incarnation_id,
                payload,
                failure,
            },
            follow_up: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_and_oversized_page_never_escape_as_a_success() {
        let cancel = Cancel(Arc::new(AtomicBool::new(true)));
        assert_eq!(
            payload(Ok(serde_json::json!({"items": []})), &cancel),
            (None, Some(CloudInventoryFailure::Unavailable))
        );
        let cancel = Cancel(Arc::new(AtomicBool::new(false)));
        assert_eq!(
            payload(
                Ok("x".repeat(koru_agent_wire::MAX_CLOUD_INVENTORY_BYTES)),
                &cancel
            ),
            (None, Some(CloudInventoryFailure::Unavailable))
        );
    }

    #[test]
    fn selectors_use_the_approved_read_uuid_and_cursor_bounds() {
        assert!(read_uuid("00000000-0000-0000-0000-000000000050").is_ok());
        assert!(read_uuid("00000000000000000000000000000050").is_err());
        assert!(read_uuid(&"x".repeat(10000)).is_err());
        assert!(validate_cursor(&Some("aB9_-".into())).is_ok());
        assert!(validate_cursor(&Some("x".repeat(2049))).is_err());
        assert!(validate_cursor(&Some("../not-a-cursor".into())).is_err());
    }
}
