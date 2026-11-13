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
