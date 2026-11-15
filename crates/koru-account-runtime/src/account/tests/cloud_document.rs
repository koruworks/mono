// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::account::cloud_document::{CloudDocumentContext, CloudDocumentPort};
use crate::cloud::{Driver, Snapshot};
use crate::enrollment::EnrollmentAuthorization;
use koru_agent_wire::{CloudDocumentSelectionPayload, CloudInspectionFailure};
use koru_store_wire::cloud_document::*;

struct DriverState(Mutex<Snapshot>);
impl Driver for DriverState {
    fn update_authorization(&self, _: EnrollmentAuthorization) -> bool {
        true
    }
    fn activate_owned(&self, _: Option<EnrollmentAuthorization>, _: u64) -> bool {
        true
    }
    fn refresh_authorization(
        &self,
        _: EnrollmentAuthorization,
        _: EnrollmentAuthorization,
    ) -> bool {
        true
    }
    fn set_foreground(&self, _: bool) {}
    fn wake(&self) -> bool {
        true
    }
    fn cancel(&self) -> bool {
        true
    }
    fn snapshot(&self) -> Snapshot {
        self.0.lock().unwrap().clone()
    }
}
struct ReadSource {
    mode: AtomicUsize,
    calls: AtomicUsize,
    driver: Arc<DriverState>,
    page: serde_json::Value,
}
impl Transport for ReadSource {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if request
            .path_and_query
            .starts_with("/v1/account/subscription")
        {
            return Ok(HttpResponse {
                status: 200,
                body: br#"{"subscriptions":[],"next_cursor":null,"cloud_access":true}"#.to_vec(),
            });
        }
        assert!(request.path_and_query.ends_with("&limit=100"));
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mode = self.mode.load(Ordering::SeqCst);
        if mode == 2 {
            return Ok(HttpResponse {
                status: 409,
                body: br#"{"code":"usage_revision_changed","message":"changed"}"#.to_vec(),
            });
        }
        if mode == 3 {
            self.driver.0.lock().unwrap().account_generation += 1;
        }
        let mut page = self.page.clone();
        if mode == 1 {
            page["items"][1]["source_head_digest"] = serde_json::json!("ab".repeat(32));
        }
        Ok(HttpResponse {
            status: 200,
            body: serde_json::to_vec(&page).unwrap(),
        })
    }
}
struct HostPort {
    calls: AtomicUsize,
    context: CloudDocumentContext,
}
impl CloudDocumentPort for HostPort {
    fn run(
        &self,
        context: CloudDocumentContext,
        selection: CloudDocumentSelection,
        action: CloudDocumentAction,
        can_write: bool,
        _: &Cancel,
    ) -> Result<CloudDocumentOutcome, CloudDocumentFailure> {
        assert_eq!(context, self.context);
        assert!(can_write);
        self.calls.fetch_add(1, Ordering::SeqCst);
        match action {
            CloudDocumentAction::Inspect => {
                Ok(CloudDocumentOutcome::View(CloudDocumentInspectionV1 {
                    document: DocumentProjection {
                        id: selection.document_id.clone(),
                        workspace_id: selection.workspace_id.clone(),
                        title: String::new(),
                        revision: 1,
                        blocks: vec![],
                    },
                    selection,
                    can_request_removal: true,
                    removable_files: vec![],
                }))
            }
            CloudDocumentAction::RemoveFile { .. } => Ok(CloudDocumentOutcome::Removed),
        }
    }
}
fn setup() -> (
    ProductionAccountService,
    Arc<ReadSource>,
    Arc<HostPort>,
    CloudDocumentSelection,
) {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../koru-account-wire/src/cloud_documents.fixtures.json"
    )))
    .unwrap();
    let page = fixtures["fixtures"][0]["response"].clone();
    let account: KoruIdentityId = "20000000-0000-4000-8000-000000000001".parse().unwrap();
    let context = CloudDocumentContext {
        owner_scope: 7,
        account_generation: 11,
    };
    let driver = Arc::new(DriverState(Mutex::new(Snapshot {
        owner_scope: Some(7),
        account_generation: 11,
        account_id: Some(account),
        ..Default::default()
    })));
    let transport = Arc::new(ReadSource {
        mode: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
        driver: driver.clone(),
        page: page.clone(),
    });
    let port = Arc::new(HostPort {
        calls: AtomicUsize::new(0),
        context,
    });
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(transport.clone()),
        signed_descriptor(),
        "test".into(),
    )
    .with_cloud(driver)
    .with_cloud_document_port(port.clone());
    service.access = Some(AccessToken("memory-only".into()));
    service.visible = Some(Arc::new(AtomicBool::new(true)));
    service.durable_owner = Some(crate::identity::AccountOwner {
        account,
        auth_identity: "20000000-0000-4000-8000-000000000002".parse().unwrap(),
    });
    let row = &page["items"][1];
    let selection = CloudDocumentSelection {
        account_id: account.to_string(),
        workspace_id: page["workspace_id"].as_str().unwrap().into(),
        incarnation_id: page["incarnation_id"].as_str().unwrap().into(),
        cloud_unit_id: row["cloud_unit_id"].as_str().unwrap().into(),
        document_id: row["document_id"].as_str().unwrap().into(),
        source_head_digest: row["source_head_digest"].as_str().unwrap().into(),
    };
    let cancel = Cancel::never();
    service
        .document_inventory(
            1,
            1,
            selection.workspace_id.clone(),
            selection.incarnation_id.clone(),
            None,
            &cancel,
        )
        .unwrap();
    (service, transport, port, selection)
}
#[test]
fn each_action_refreshes_exact_page_and_stale_head_cursor_or_generation_never_reaches_host() {
    for mode in 0..4 {
        let (mut service, transport, port, selection) = setup();
        transport.mode.store(mode, Ordering::SeqCst);
        let result = service
            .cloud_document_command(
                2,
                2,
                CloudDocumentSelectionPayload::new(serde_json::to_string(&selection).unwrap())
                    .unwrap(),
                None,
                &Cancel::never(),
            )
            .unwrap()
            .result;
        let AccountOperationResult::CloudDocumentInspection {
            payload, failure, ..
        } = result
        else {
            panic!()
        };
        assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
        assert_eq!(port.calls.load(Ordering::SeqCst), usize::from(mode == 0));
        assert_eq!(payload.is_some(), mode == 0);
        assert_eq!(
            failure,
            match mode {
                0 => None,
                1 | 2 => Some(CloudInspectionFailure::SourceChanged),
                _ => Some(CloudInspectionFailure::Unavailable),
            }
        );
    }
}
#[test]
fn altered_unit_account_or_old_same_account_context_cannot_retarget_cached_selection() {
    for field in 0..3 {
        let (mut service, transport, port, mut selection) = setup();
        match field {
            0 => selection.cloud_unit_id = "00000000-0000-0000-0000-000000009999".into(),
            1 => selection.account_id = "20000000-0000-4000-8000-000000000099".into(),
            _ => transport.driver.0.lock().unwrap().account_generation += 1,
        }
        let result = service
            .cloud_document_command(
                2,
                2,
                CloudDocumentSelectionPayload::new(serde_json::to_string(&selection).unwrap())
                    .unwrap(),
                None,
                &Cancel::never(),
            )
            .unwrap()
            .result;
        assert!(matches!(
            result,
            AccountOperationResult::CloudDocumentInspection {
                payload: None,
                failure: Some(_),
                ..
            }
        ));
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(port.calls.load(Ordering::SeqCst), 0);
    }
}
#[test]
fn acknowledged_removal_invalidates_selection_until_inventory_is_read_again() {
    let (mut service, transport, port, selection) = setup();
    let encoded =
        CloudDocumentSelectionPayload::new(serde_json::to_string(&selection).unwrap()).unwrap();
    let result = service
        .cloud_document_command(
            2,
            2,
            encoded.clone(),
            Some("00000000-0000-0000-0000-000000001234".into()),
            &Cancel::never(),
        )
        .unwrap()
        .result;
    assert!(matches!(
        result,
        AccountOperationResult::CloudFileRemoval {
            removed: true,
            failure: None,
            ..
        }
    ));
    let result = service
        .cloud_document_command(3, 3, encoded, None, &Cancel::never())
        .unwrap()
        .result;
    assert!(matches!(
        result,
        AccountOperationResult::CloudDocumentInspection {
            failure: Some(CloudInspectionFailure::SourceChanged),
            ..
        }
    ));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
}
