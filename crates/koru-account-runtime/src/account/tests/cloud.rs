// SPDX-License-Identifier: MPL-2.0

use super::*;

struct Billing(bool);
impl Transport for Billing {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        assert_eq!(request.path_and_query, "/v1/account/subscription?limit=25");
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name == "authorization" && value == "Bearer memory-only")
        );
        Ok(HttpResponse {
            document_removal: false,
            status: if self.0 { 200 } else { 503 },
            body: if self.0 {
                br#"{"subscriptions":[{"status":"past_due"}],"next_cursor":null}"#.to_vec()
            } else {
                br#"{"error":{"code":"billing_unavailable"}}"#.to_vec()
            },
        })
    }
}
#[test]
fn billing_outage_is_targeted_and_preserves_account_and_credentials() {
    for available in [false, true] {
        let mut service = ProductionAccountService::with_client(
            AccountClient::with_transport(Arc::new(Billing(available))),
            signed_descriptor(),
            "test".into(),
        );
        service.access = Some(AccessToken("memory-only".into()));
        service.access_expires_at_millis = Some(2_000_000_000_000);
        let restored = SessionCredential::new(
            "20000000-0000-4000-8000-000000000002".into(),
            "refresh-vault-only".into(),
        )
        .unwrap();
        let cancel = Cancel(Arc::new(AtomicBool::new(false)));
        let outcome = service
            .execute(
                70,
                AccountCommand::InspectCloudSubscription { request_id: 42 },
                Some(&restored),
                &cancel,
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(outcome.state.stage, AccountStage::SignedIn);
        assert!(matches!(outcome.credential, CredentialChange::Keep));
        assert_eq!(
            outcome.result,
            AccountOperationResult::CloudSubscription {
                request_id: 42,
                current: available.then_some(true),
                access: available.then_some(true),
            }
        );
        assert_eq!(service.access.as_ref().unwrap().0, "memory-only");
    }
}
#[test]
fn worker_failures_retain_cloud_request_identity_before_admission() {
    for code in [
        "account_retry_pending",
        "account_retry_exhausted",
        "authentication_required",
        "cancelled",
    ] {
        assert_eq!(
            command_failure(
                &AccountCommand::InspectCloudSubscription { request_id: 91 },
                code
            ),
            AccountOperationResult::CloudSubscription {
                request_id: 91,
                current: None,
                access: None,
            }
        );
        assert!(matches!(
            command_failure(&AccountCommand::ListProfiles, code),
            AccountOperationResult::Failed { .. } | AccountOperationResult::Cancelled
        ));
    }
}

struct StoragePlan(bool);
impl Transport for StoragePlan {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        assert_eq!(request.path_and_query, "/v1/account/cloud-plan");
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name == "authorization" && value == "Bearer memory-only")
        );
        let fixtures: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../koru-account-wire/src/cloud_plan.fixtures.json"
        )))
        .unwrap();
        Ok(HttpResponse {
            document_removal: false,
            status: if self.0 { 200 } else { 503 },
            body: if self.0 {
                serde_json::to_vec(&fixtures["fixtures"][0]["response"]).unwrap()
            } else {
                Vec::new()
            },
        })
    }
}

#[test]
fn storage_read_is_targeted_typed_and_preserves_sign_in_on_failure() {
    for available in [true, false] {
        let mut service = ProductionAccountService::with_client(
            AccountClient::with_transport(Arc::new(StoragePlan(available))),
            signed_descriptor(),
            "test".into(),
        );
        service.access = Some(AccessToken("memory-only".into()));
        service.access_expires_at_millis = Some(2_000_000_000_000);
        let restored = SessionCredential::new(
            "20000000-0000-4000-8000-000000000002".into(),
            "refresh-vault-only".into(),
        )
        .unwrap();
        let outcome = service
            .execute(
                70,
                AccountCommand::InspectCloudPlan { request_id: 42 },
                Some(&restored),
                &Cancel(Arc::new(AtomicBool::new(false))),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(outcome.state.stage, AccountStage::SignedIn);
        assert!(matches!(outcome.credential, CredentialChange::Keep));
        let AccountOperationResult::CloudPlan {
            request_id,
            payload,
            ..
        } = outcome.result
        else {
            panic!()
        };
        assert_eq!(request_id, 42);
        assert_eq!(payload.is_some(), available);
        if let Some(payload) = payload {
            let _: koru_account_wire::cloud_plan::CloudPlanResponseV1 =
                serde_json::from_str(payload.as_str()).unwrap();
        }
    }
    assert_eq!(
        command_failure(
            &AccountCommand::InspectCloudPlan { request_id: 43 },
            "authentication_required"
        ),
        AccountOperationResult::CloudPlan {
            request_id: 43,
            payload: None,
            request_elapsed_millis: 0
        }
    );
}

struct InventoryRead {
    documents: bool,
    status: u16,
    response: Vec<u8>,
}
impl Transport for InventoryRead {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let expected = if self.documents {
            "/v1/account/cloud-usage/workspaces/00000000-0000-0000-0000-000000000050/documents?incarnation_id=00000000-0000-0000-0000-000000000051&limit=100"
        } else {
            "/v1/account/cloud-usage/workspaces?limit=100"
        };
        assert_eq!(request.path_and_query, expected);
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name == "authorization" && value == "Bearer memory-only")
        );
        Ok(HttpResponse {
            document_removal: false,
            status: self.status,
            body: self.response.clone(),
        })
    }
}
#[test]
fn inventory_pages_are_typed_exact_scoped_and_revision_failure_never_means_empty() {
    use koru_agent_wire::CloudInventoryFailure;
    for documents in [false, true] {
        for status in [200, 409, 503] {
            let fixtures: serde_json::Value = serde_json::from_str(if documents {
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../koru-account-wire/src/cloud_documents.fixtures.json"
                ))
            } else {
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../koru-account-wire/src/cloud_plan.fixtures.json"
                ))
            })
            .unwrap();
            let page = if documents {
                fixtures["fixtures"][0]["response"].clone()
            } else {
                fixtures["fixtures"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["name"] == "workspace_page")
                    .unwrap()["response"]
                    .clone()
            };
            let body = match status {
                200 => serde_json::to_vec(&page).unwrap(),
                409 => br#"{"code":"usage_revision_changed","message":"changed"}"#.to_vec(),
                _ => Vec::new(),
            };
            let mut service = ProductionAccountService::with_client(
                AccountClient::with_transport(Arc::new(InventoryRead {
                    documents,
                    status,
                    response: body,
                })),
                signed_descriptor(),
                "test".into(),
            );
            service.access = Some(AccessToken("memory-only".into()));
            service.access_expires_at_millis = Some(2_000_000_000_000);
            let restored = SessionCredential::new(
                "20000000-0000-4000-8000-000000000002".into(),
                "refresh-vault-only".into(),
            )
            .unwrap();
            let command = if documents {
                AccountCommand::InspectCloudUsageDocuments {
                    request_id: 42,
                    workspace_id: "00000000-0000-0000-0000-000000000050".into(),
                    incarnation_id: "00000000-0000-0000-0000-000000000051".into(),
                    cursor: None,
                }
            } else {
                AccountCommand::InspectCloudUsageWorkspaces {
                    request_id: 42,
                    cursor: None,
                }
            };
            let outcome = service
                .execute(
                    70,
                    command.clone(),
                    Some(&restored),
                    &Cancel(Arc::new(AtomicBool::new(false))),
                    &mut |_| {},
                )
                .unwrap();
            assert_eq!(outcome.state.stage, AccountStage::SignedIn);
            assert!(matches!(outcome.credential, CredentialChange::Keep));
            let (request_id, payload, failure) = match outcome.result {
                AccountOperationResult::CloudUsageWorkspaces {
                    request_id,
                    payload,
                    failure,
                } => (request_id, payload, failure),
                AccountOperationResult::CloudUsageDocuments {
                    request_id,
                    workspace_id,
                    incarnation_id,
                    payload,
                    failure,
                } => {
                    assert_eq!(workspace_id, "00000000-0000-0000-0000-000000000050");
                    assert_eq!(incarnation_id, "00000000-0000-0000-0000-000000000051");
                    (request_id, payload, failure)
                }
                _ => panic!(),
            };
            assert_eq!(request_id, 42);
            assert_eq!(
                failure,
                match status {
                    200 => None,
                    409 => Some(CloudInventoryFailure::RevisionChanged),
                    _ => Some(CloudInventoryFailure::Unavailable),
                }
            );
            assert_eq!(payload.is_some(), status == 200);
            if let Some(payload) = payload {
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(payload.as_str()).unwrap(),
                    page
                );
            }
            assert!(matches!(
                command_failure(&command, "cancelled"),
                AccountOperationResult::CloudUsageWorkspaces { payload: None, .. }
                    | AccountOperationResult::CloudUsageDocuments { payload: None, .. }
            ));
        }
    }
}
