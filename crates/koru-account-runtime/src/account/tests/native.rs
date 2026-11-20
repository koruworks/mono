// SPDX-License-Identifier: MPL-2.0

use super::*;
mod challenge;
#[path = "creation.rs"]
mod creation;
mod switching;
use crate::delivery::{NativeSignInRequest, NativeSignInSession, SignInDelivery, SignInSession};
use koru_account_wire::{
    AccountCreationConfirmationV1, AccountCreationResponseV1, GoogleIdToken,
    GoogleNativeTokenCompleteRequestV1, GoogleNativeTokenCompleteResponseV1,
    GoogleNativeTokenStartResponseV1,
};

const CREATION_NOW: i64 = 1_900_000_000_000;
struct CreationClock(Instant);
impl Clock for CreationClock {
    fn now_millis(&self) -> Option<i64> {
        Some(CREATION_NOW)
    }
    fn monotonic(&self) -> Instant {
        self.0
    }
}
struct CreationEntropy;
impl Entropy for CreationEntropy {
    fn fill_bytes(&self, bytes: &mut [u8]) {
        bytes.fill(19);
    }
}

#[derive(Default)]
struct CreationTransport {
    returning: bool,
    cancel_after_complete: Option<Arc<AtomicBool>>,
    base: TokenTransport,
    decisions: Mutex<Vec<(String, Vec<u8>)>>,
    lose_decision: AtomicUsize,
    expired: AtomicBool,
    first_decision_gate: Option<(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>)>,
    committed_decision: Mutex<Option<bool>>,
    logouts: AtomicUsize,
    uncertain_response: AtomicUsize,
    fault_exchange: bool,
    expired_challenge: AtomicBool,
    expired_exchange: AtomicBool,
    logout_requests: Mutex<Vec<LogoutRequestV1>>,
    logout_authorizations: Mutex<Vec<Option<String>>>,
}
impl Transport for CreationTransport {
    // `fetch_update` is deprecated for `try_update`, which is unstable on the
    // Rust 1.93 the Linux API image builds with; rename when that image moves.
    #[allow(deprecated)]
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if request.path_and_query.ends_with("/native/device-challenge")
            && self.expired_challenge.load(Ordering::Acquire)
        {
            return Ok(expired_creation_response());
        }
        if request.path_and_query.ends_with("/native/exchange")
            && self.expired_exchange.load(Ordering::Acquire)
        {
            self.base.exchanges.lock().unwrap().push(request.body);
            return Ok(expired_creation_response());
        }
        if request.path_and_query.ends_with("/native/token/start") {
            self.base.starts.fetch_add(1, Ordering::AcqRel);
            let mut start = native_start(false);
            start.expires_at_millis = CREATION_NOW + 300_000;
            return Ok(HttpResponse {
                document_removal: false,
                status: 200,
                body: serde_json::to_vec(&start).unwrap(),
            });
        }
        if request.path_and_query.ends_with("/native/token/complete") {
            if self.returning {
                return self.base.send(request);
            }
            self.base.completes.lock().unwrap().push(request.body);
            if let Some(cancel) = &self.cancel_after_complete {
                cancel.store(true, Ordering::Release);
            }
            return Ok(HttpResponse {
                document_removal: false,
                status: 200,
                body: serde_json::to_vec(
                    &GoogleNativeTokenCompleteResponseV1::ConfirmationRequired {
                        version: Version1,
                        confirmation: AccountCreationConfirmationV1 {
                            authority: OneUseGrant("confirmation-authority".into()),
                            expires_at_millis: CREATION_NOW + 300_000,
                        },
                    },
                )
                .unwrap(),
            });
        }
        if request.path_and_query.ends_with("/account/confirm")
            || request.path_and_query.ends_with("/account/cancel")
        {
            let confirm = request.path_and_query.ends_with("/confirm");
            self.decisions
                .lock()
                .unwrap()
                .push((request.path_and_query, request.body));
            let confirm = *self
                .committed_decision
                .lock()
                .unwrap()
                .get_or_insert(confirm);
            if !self.fault_exchange
                && self.decisions.lock().unwrap().len() == 1
                && let Some((entered, release)) = &self.first_decision_gate
            {
                entered.send(()).unwrap();
                release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
            }
            if !self.fault_exchange
                && let Some(response) = self.take_uncertain_response()
            {
                return Ok(response);
            }
            if self
                .lose_decision
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                .is_ok()
            {
                return Err(TransportError::Unavailable);
            }
            if self.expired.load(Ordering::Acquire) {
                return Ok(HttpResponse {
                    document_removal: false,
                    status: 410,
                    body: serde_json::to_vec(&koru_account_wire::DeveloperErrorV1 {
                        retry_after_millis: None,
                        code: koru_account_wire::ErrorCode::parse("flow_expired").unwrap(),
                        message: "fixture".into(),
                    })
                    .unwrap(),
                });
            }
            let result = if confirm {
                AccountCreationResponseV1::Native {
                    completion: GoogleNativeTokenCompleteResponseV1::ExchangeReady {
                        version: Version1,
                        flow_id: native_start(false).flow_id,
                        grant: OneUseGrant("50000000-0000-4000-8000-000000000005.one-use".into()),
                        expires_at_millis: CREATION_NOW + 300_000,
                    },
                }
            } else {
                AccountCreationResponseV1::Cancelled { version: Version1 }
            };
            return Ok(HttpResponse {
                document_removal: false,
                status: 200,
                body: serde_json::to_vec(&result).unwrap(),
            });
        }
        if request.path_and_query.ends_with("/logout") {
            self.logout_authorizations.lock().unwrap().push(
                request
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                    .map(|(_, value)| value.clone()),
            );
            self.logouts.fetch_add(1, Ordering::AcqRel);
            self.logout_requests
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&request.body).unwrap());
        }
        let exchange = request.path_and_query.ends_with("/native/exchange");
        let response = self.base.send(request);
        if exchange && self.fault_exchange && self.base.exchanges.lock().unwrap().len() == 1 {
            if let Some((entered, release)) = &self.first_decision_gate {
                entered.send(()).unwrap();
                release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
            }
            if let Some(response) = self.take_uncertain_response() {
                return Ok(response);
            }
        }
        response
    }
}
fn expired_creation_response() -> HttpResponse {
    HttpResponse {
        document_removal: false,
        status: 410,
        body: serde_json::to_vec(&koru_account_wire::DeveloperErrorV1 {
            retry_after_millis: None,
            code: koru_account_wire::ErrorCode::parse("flow_expired").unwrap(),
            message: "fixture".into(),
        })
        .unwrap(),
    }
}
impl CreationTransport {
    fn take_uncertain_response(&self) -> Option<HttpResponse> {
        match self.uncertain_response.swap(0, Ordering::AcqRel) {
            1 => Some(HttpResponse {
                document_removal: false,
                status: 200,
                body: b"{\"version\":".to_vec(),
            }),
            2 => Some(HttpResponse {
                document_removal: false,
                status: 408,
                body: serde_json::to_vec(&koru_account_wire::DeveloperErrorV1 {
                    retry_after_millis: None,
                    code: koru_account_wire::ErrorCode::parse("flow_unavailable").unwrap(),
                    message: "fixture".into(),
                })
                .unwrap(),
            }),
            _ => None,
        }
    }
}
fn creation_service(
    transport: Arc<CreationTransport>,
    delivery: Arc<TokenDelivery>,
) -> ProductionAccountService {
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(transport),
        signed_descriptor(),
        "test".into(),
    );
    service.delivery = delivery;
    service.clock = Arc::new(CreationClock(Instant::now()));
    service.entropy = Arc::new(CreationEntropy);
    service
}
fn decide(
    service: &mut ProductionAccountService,
    command: AccountCommand,
    cancelled: bool,
) -> Result<ServiceOutcome, ServiceError> {
    service.execute(
        8,
        command,
        None,
        &Cancel(Arc::new(AtomicBool::new(cancelled))),
        &mut |_| {},
    )
}

#[test]
fn creation_automatically_uses_one_google_proof_and_keeps_exact_exchange_until_vault_ack() {
    let transport = Arc::new(CreationTransport::default());
    let delivery = Arc::new(TokenDelivery::default());
    let mut service = creation_service(transport.clone(), delivery.clone());
    let result = execute_native(&mut service).unwrap();
    assert_eq!(result.state.stage, AccountStage::SignedIn);
    assert!(matches!(result.result, AccountOperationResult::SignedIn));
    assert_eq!(transport.base.exchanges.lock().unwrap().len(), 1);
    assert_eq!(delivery.prompts.lock().unwrap().len(), 1);
    assert!(matches!(result.credential, CredentialChange::Replace(_)));
    assert!(!service.cancellation_safe());
    let again = decide(&mut service, AccountCommand::Retry, false).unwrap();
    assert!(matches!(again.result, AccountOperationResult::SignedIn));
    assert_eq!(transport.decisions.lock().unwrap().len(), 1);
    assert_eq!(delivery.prompts.lock().unwrap().len(), 1);
    let exchanges = transport.base.exchanges.lock().unwrap();
    assert_eq!(exchanges.len(), 2);
    assert_eq!(exchanges[0], exchanges[1]);
    drop(exchanges);
    service.credential_acknowledged();
    assert!(service.pending_creation.is_none());
}

#[test]
fn creation_cancel_after_provider_proof_never_confirms_exchanges_or_repeats_google() {
    let cancel = Arc::new(AtomicBool::new(false));
    let transport = Arc::new(CreationTransport {
        cancel_after_complete: Some(cancel.clone()),
        ..Default::default()
    });
    let delivery = Arc::new(TokenDelivery::default());
    let mut service = creation_service(transport.clone(), delivery.clone());
    assert!(matches!(
        service
            .execute(
                8,
                AccountCommand::StartGoogle {
                    locale: "en".into()
                },
                None,
                &Cancel(cancel),
                &mut |_| {}
            )
            .unwrap()
            .result,
        AccountOperationResult::Cancelled
    ));
    assert!(
        transport.decisions.lock().unwrap()[0]
            .0
            .ends_with("/cancel")
    );
    assert!(transport.base.exchanges.lock().unwrap().is_empty());
    assert_eq!(delivery.prompts.lock().unwrap().len(), 1);
    assert!(service.cancellation_safe());
}

#[test]
fn lost_creation_reply_retains_chosen_request_even_when_opposite_intent_arrives() {
    for confirm in [false, true] {
        let cancel = Arc::new(AtomicBool::new(false));
        let transport = Arc::new(CreationTransport {
            cancel_after_complete: (!confirm).then(|| cancel.clone()),
            ..Default::default()
        });
        transport.lose_decision.store(1, Ordering::Release);
        let delivery = Arc::new(TokenDelivery::default());
        let mut service = creation_service(transport.clone(), delivery.clone());
        let opposite = if confirm {
            AccountCommand::CancelAccountCreation
        } else {
            AccountCommand::ConfirmAccountCreation
        };
        assert!(
            service
                .execute(
                    8,
                    AccountCommand::StartGoogle {
                        locale: "en".into()
                    },
                    None,
                    &Cancel(cancel),
                    &mut |_| {}
                )
                .err()
                .unwrap()
                .retryable
        );
        assert!(!service.cancellation_safe());
        decide(&mut service, opposite, false).unwrap();
        let decisions = transport.decisions.lock().unwrap();
        assert_eq!(decisions.len(), 2);
        assert_eq!(decisions[0], decisions[1]);
        assert_eq!(delivery.prompts.lock().unwrap().len(), 1);
    }
}

#[test]
fn expired_creation_releases_unusable_proof_without_claiming_cancellation() {
    let transport = Arc::new(CreationTransport::default());
    transport.lose_decision.store(1, Ordering::Release);
    let delivery = Arc::new(TokenDelivery::default());
    let mut service = creation_service(transport.clone(), delivery);
    assert!(execute_native(&mut service).is_err());
    transport.expired.store(true, Ordering::Release);
    let result = decide(&mut service, AccountCommand::Retry, false).unwrap();
    assert_eq!(result.state.stage, AccountStage::SignedOut);
    assert_eq!(
        result.state.failure_code.as_deref(),
        Some("account_confirmation_expired")
    );
    assert!(matches!(result.credential, CredentialChange::Keep));
    assert!(matches!(
        result.result,
        AccountOperationResult::Failed { .. }
    ));
    assert!(service.cancellation_safe());
    assert!(service.pending_native.is_none());
    transport.expired.store(false, Ordering::Release);
    assert_eq!(
        execute_native(&mut service).unwrap().state.stage,
        AccountStage::SignedIn
    );
    assert_eq!(transport.base.starts.load(Ordering::Acquire), 2);
}

#[test]
fn logout_resolves_uncertain_confirmation_before_revoking_the_exchange() {
    let transport = Arc::new(CreationTransport::default());
    transport.lose_decision.store(1, Ordering::Release);
    let delivery = Arc::new(TokenDelivery::default());
    let mut service = creation_service(transport.clone(), delivery.clone());
    assert!(execute_native(&mut service).is_err());
    let result = decide(&mut service, AccountCommand::Logout, true).unwrap();
    assert!(matches!(result.result, AccountOperationResult::SignedOut));
    assert!(matches!(result.credential, CredentialChange::Remove));
    let decisions = transport.decisions.lock().unwrap();
    assert_eq!(decisions[0], decisions[1]);
    assert!(!transport.base.exchanges.lock().unwrap().is_empty());
    assert_eq!(delivery.prompts.lock().unwrap().len(), 1);
    assert!(service.cancellation_safe());
}

#[derive(Default)]
struct TokenDelivery {
    prompts: Arc<Mutex<Vec<NativeSignInRequest>>>,
    cancel: bool,
}
impl SignInDelivery for TokenDelivery {
    fn bind(&self) -> Result<Box<dyn SignInSession>, ServiceError> {
        panic!("native delivery must never bind a browser listener")
    }
    fn supports_native_token(&self) -> bool {
        true
    }
    fn bind_native(&self) -> Result<Box<dyn NativeSignInSession>, ServiceError> {
        Ok(Box::new(TokenSession {
            prompts: self.prompts.clone(),
            cancel: self.cancel,
        }))
    }
}
struct TokenSession {
    prompts: Arc<Mutex<Vec<NativeSignInRequest>>>,
    cancel: bool,
}
impl NativeSignInSession for TokenSession {
    fn wait(
        &mut self,
        request: &NativeSignInRequest,
        _: &Cancel,
    ) -> Result<GoogleIdToken, ServiceError> {
        self.prompts.lock().unwrap().push(request.clone());
        if self.cancel {
            return Err(ServiceError::terminal("cancelled"));
        }
        Ok(GoogleIdToken(format!("token-for-{}", request.nonce)))
    }
}
struct TokenTransport {
    completes: Mutex<Vec<Vec<u8>>>,
    starts: AtomicUsize,
    exchanges: Mutex<Vec<Vec<u8>>>,
    lose_complete: AtomicUsize,
    lose_exchange: AtomicUsize,
    collision: bool,
    first_complete_gate: Option<(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>)>,
    completion_unavailable: AtomicBool,
}
impl Default for TokenTransport {
    fn default() -> Self {
        Self {
            completes: Mutex::new(Vec::new()),
            starts: AtomicUsize::new(0),
            exchanges: Mutex::new(Vec::new()),
            lose_complete: AtomicUsize::new(0),
            lose_exchange: AtomicUsize::new(0),
            collision: false,
            first_complete_gate: None,
            completion_unavailable: AtomicBool::new(false),
        }
    }
}
fn native_start(reauth: bool) -> GoogleNativeTokenStartResponseV1 {
    GoogleNativeTokenStartResponseV1 {
        version: Version1,
        flow_id: if reauth {
            "60000000-0000-4000-8000-000000000006"
        } else {
            "10000000-0000-4000-8000-000000000001"
        }
        .parse()
        .unwrap(),
        flow_state: OAuthState(
            if reauth {
                "second-state"
            } else {
                "first-state"
            }
            .into(),
        ),
        nonce: OAuthState(
            if reauth {
                "second-nonce"
            } else {
                "first-nonce"
            }
            .into(),
        ),
        expires_at_millis: SystemClock.now_millis().unwrap() + 300_000,
    }
}
impl Transport for TokenTransport {
    // `fetch_update` is deprecated for `try_update`, which is unstable on the
    // Rust 1.93 the Linux API image builds with; rename when that image moves.
    #[allow(deprecated)]
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if request.path_and_query.ends_with("/native/token/start") {
            self.starts.fetch_add(1, Ordering::AcqRel);
            return Ok(HttpResponse {
                document_removal: false,
                status: 200,
                body: serde_json::to_vec(&native_start(false)).unwrap(),
            });
        }
        if request.path_and_query.ends_with("/native/token/complete") {
            let complete: GoogleNativeTokenCompleteRequestV1 =
                serde_json::from_slice(&request.body).unwrap();
            self.completes.lock().unwrap().push(request.body);
            if self.completes.lock().unwrap().len() == 1
                && let Some((entered, release)) = &self.first_complete_gate
            {
                entered.send(()).unwrap();
                release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
            }
            if self.completion_unavailable.load(Ordering::Acquire) {
                return Err(TransportError::Unavailable);
            }
            if self
                .lose_complete
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                .is_ok()
            {
                return Err(TransportError::Unavailable);
            }
            let response = if self.collision && complete.flow_id == native_start(false).flow_id {
                assert_eq!(complete.id_token.0, "token-for-first-nonce");
                GoogleNativeTokenCompleteResponseV1::CollisionRequired {
                    version: Version1,
                    reauthentication: native_start(true),
                }
            } else {
                assert_eq!(
                    complete.id_token.0,
                    if self.collision {
                        "token-for-second-nonce"
                    } else {
                        "token-for-first-nonce"
                    }
                );
                GoogleNativeTokenCompleteResponseV1::ExchangeReady {
                    version: Version1,
                    flow_id: complete.flow_id,
                    grant: OneUseGrant("50000000-0000-4000-8000-000000000005.one-use".into()),
                    expires_at_millis: native_start(false).expires_at_millis,
                }
            };
            return Ok(HttpResponse {
                document_removal: false,
                status: 200,
                body: serde_json::to_vec(&response).unwrap(),
            });
        }
        if request.path_and_query.ends_with("/native/exchange") {
            self.exchanges.lock().unwrap().push(request.body.clone());
            if self
                .lose_exchange
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                .is_ok()
            {
                return Err(TransportError::Unavailable);
            }
        }
        if request.path_and_query.ends_with("/logout") {
            return Ok(HttpResponse {
                document_removal: false,
                status: 200,
                body: serde_json::to_vec(&koru_account_wire::EmptyResponseV1 { version: Version1 })
                    .unwrap(),
            });
        }
        NativeFlowTransport.send(request)
    }
}
fn native_service(
    transport: Arc<TokenTransport>,
    delivery: Arc<TokenDelivery>,
) -> ProductionAccountService {
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(transport),
        signed_descriptor(),
        "test".into(),
    );
    service.delivery = delivery;
    service
}
fn execute_native(service: &mut ProductionAccountService) -> Result<ServiceOutcome, ServiceError> {
    service.execute(
        7,
        AccountCommand::StartGoogle {
            locale: "en".into(),
        },
        None,
        &Cancel(Arc::new(AtomicBool::new(false))),
        &mut |_| {},
    )
}

#[test]
fn native_lost_complete_and_exchange_replay_exactly_until_vault_ack() {
    let transport = Arc::new(TokenTransport::default());
    transport.lose_complete.store(1, Ordering::Release);
    transport.lose_exchange.store(1, Ordering::Release);
    let delivery = Arc::new(TokenDelivery::default());
    let mut service = native_service(transport.clone(), delivery.clone());
    assert!(execute_native(&mut service).err().unwrap().retryable);
    assert!(!service.cancellation_safe());
    assert!(execute_native(&mut service).err().unwrap().retryable);
    let outcome = execute_native(&mut service).unwrap();
    assert!(matches!(outcome.credential, CredentialChange::Replace(_)));
    assert_eq!(delivery.prompts.lock().unwrap().len(), 1);
    assert_eq!(transport.starts.load(Ordering::Acquire), 1);
    let completes = transport.completes.lock().unwrap();
    assert_eq!(completes.len(), 2);
    assert_eq!(completes[0], completes[1]);
    let exchanges = transport.exchanges.lock().unwrap();
    assert_eq!(exchanges.len(), 2);
    assert_eq!(exchanges[0], exchanges[1]);
    let exchange: NativeExchangeRequestV1 = serde_json::from_slice(&exchanges[0]).unwrap();
    assert!(exchange.device_ownership_proof.is_some());
    assert!(!service.cancellation_safe());
    service.credential_acknowledged();
    assert!(service.cancellation_safe());
    assert!(service.pending_native.is_none());
}

#[test]
fn native_collision_requests_one_fresh_correlated_sheet() {
    let transport = Arc::new(TokenTransport {
        collision: true,
        ..Default::default()
    });
    let delivery = Arc::new(TokenDelivery::default());
    let mut service = native_service(transport.clone(), delivery.clone());
    execute_native(&mut service).unwrap();
    let prompts = delivery.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 2);
    assert!(!prompts[0].reauthentication);
    assert!(prompts[1].reauthentication);
    assert_ne!(prompts[0].nonce, prompts[1].nonce);
    assert_ne!(prompts[0].request_id, prompts[1].request_id);
    assert_eq!(prompts[1].generation, 7);
    let completes = transport.completes.lock().unwrap();
    let first: GoogleNativeTokenCompleteRequestV1 = serde_json::from_slice(&completes[0]).unwrap();
    let second: GoogleNativeTokenCompleteRequestV1 = serde_json::from_slice(&completes[1]).unwrap();
    assert_eq!(first.client_state, second.client_state);
    assert_eq!(first.pkce_verifier, second.pkce_verifier);
    assert_ne!(first.flow_id, second.flow_id);
}

#[test]
fn native_sheet_cancel_has_no_completion_or_exchange() {
    let transport = Arc::new(TokenTransport::default());
    let delivery = Arc::new(TokenDelivery {
        cancel: true,
        ..Default::default()
    });
    let mut service = native_service(transport.clone(), delivery);
    assert_eq!(
        execute_native(&mut service).err().unwrap().code,
        "cancelled"
    );
    assert!(service.cancellation_safe());
    assert!(transport.completes.lock().unwrap().is_empty());
    assert!(transport.exchanges.lock().unwrap().is_empty());
    service.cancelled(7);
    assert!(service.pending_native.is_none());
}

#[test]
fn native_cancel_during_outage_keeps_exact_proof_until_recovery() {
    let transport = Arc::new(TokenTransport::default());
    transport.lose_complete.store(2, Ordering::Release);
    let delivery = Arc::new(TokenDelivery::default());
    let mut service = native_service(transport.clone(), delivery.clone());
    assert!(execute_native(&mut service).err().unwrap().retryable);
    let cancel = Cancel(Arc::new(AtomicBool::new(true)));
    assert!(
        service
            .execute(7, AccountCommand::Logout, None, &cancel, &mut |_| {})
            .err()
            .unwrap()
            .retryable
    );
    assert!(!service.cancellation_safe());
    service
        .execute(7, AccountCommand::Logout, None, &cancel, &mut |_| {})
        .unwrap();
    assert!(service.cancellation_safe());
    assert_eq!(delivery.prompts.lock().unwrap().len(), 1);
    let completes = transport.completes.lock().unwrap();
    assert_eq!(completes.len(), 3);
    assert_eq!(completes[0], completes[1]);
    assert_eq!(completes[1], completes[2]);
}

#[test]
fn worker_cancel_cannot_report_signed_out_while_exact_recovery_is_unavailable() {
    let (entered, in_flight) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let transport = Arc::new(TokenTransport {
        first_complete_gate: Some((entered, Mutex::new(wait))),
        ..Default::default()
    });
    transport
        .completion_unavailable
        .store(true, Ordering::Release);
    let delivery = Arc::new(TokenDelivery::default());
    let store = Arc::new(SessionCredentialStore::default());
    let (notify, events) = mpsc::channel();
    let (unavailable, failure_published) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let observed = transport.clone();
    let stopped_at_failure = AtomicBool::new(false);
    let control = AccountControl::start(
        native_service(transport.clone(), delivery.clone()),
        store.clone(),
        entry(),
        move |event| {
            // Cancel both raises the in-flight flag and queues a wake. The
            // initial request, inline cancel recovery, and queued cancel wake
            // each encounter the held outage. Observe the published failure
            // after all three, before allowing any successful replay.
            let failed_third_completion = matches!(event, WorkerEvent::Changed)
                && observed.completes.lock().unwrap().len() == 3
                && !stopped_at_failure.swap(true, Ordering::AcqRel);
            let _ = notify.send(event);
            if failed_third_completion {
                unavailable.send(()).unwrap();
                resumed.recv_timeout(Duration::from_secs(2)).unwrap();
            }
        },
    );
    let generation = control
        .enqueue(
            8,
            AccountCommand::StartGoogle {
                locale: "en".into(),
            },
        )
        .unwrap();
    in_flight.recv_timeout(Duration::from_secs(2)).unwrap();
    control
        .enqueue(8, AccountCommand::Cancel { generation })
        .unwrap();
    release.send(()).unwrap();
    failure_published
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    assert_eq!(control.state().stage, AccountStage::Failed);
    assert!(control.state().retry_at_millis.is_some());
    assert_eq!(transport.completes.lock().unwrap().len(), 3);
    assert!(events.try_iter().all(|event| !matches!(
        event,
        WorkerEvent::Result {
            result: AccountOperationResult::SignedOut,
            ..
        }
    )));
    transport
        .completion_unavailable
        .store(false, Ordering::Release);
    let retry = control.enqueue(8, AccountCommand::Retry).unwrap();
    resume.send(()).unwrap();
    assert!(matches!(
        terminal_result(&events, 8, retry),
        AccountOperationResult::Cancelled
    ));
    assert_eq!(delivery.prompts.lock().unwrap().len(), 1);
    assert!(store.load(&entry()).unwrap().is_none());
    let completes = transport.completes.lock().unwrap();
    assert_eq!(completes.len(), 4);
    assert_eq!(completes[0], completes[1]);
    assert_eq!(completes[1], completes[2]);
    assert_eq!(completes[2], completes[3]);
}

struct LoadProbe(mpsc::Sender<()>);
impl CredentialStore for LoadProbe {
    fn load(
        &self,
        _: &CredentialEntry,
    ) -> Result<Option<koru_oauth_credentials::SecretString>, Error> {
        self.0.send(()).unwrap();
        Ok(None)
    }
    fn store(
        &self,
        _: &CredentialEntry,
        _: koru_oauth_credentials::SecretString,
    ) -> Result<(), Error> {
        Ok(())
    }
    fn clear(&self, _: &CredentialEntry) -> Result<(), Error> {
        Ok(())
    }
}

#[test]
fn initial_background_defers_vault_and_drop_releases_parked_worker() {
    let (loaded, loads) = mpsc::channel();
    let control = AccountControl::start_with_clock_and_foreground(
        CredentialService,
        Arc::new(LoadProbe(loaded)),
        entry(),
        Arc::new(SystemClock),
        false,
        |_| {},
    );
    assert!(loads.recv_timeout(Duration::from_millis(30)).is_err());
    control.set_foreground(true);
    loads.recv_timeout(Duration::from_secs(2)).unwrap();
    control.set_foreground(false);
    drop(control);
    assert!(matches!(
        loads.recv_timeout(Duration::from_secs(2)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
}

#[test]
fn dropping_initial_background_exits_without_loading_the_vault() {
    let (loaded, loads) = mpsc::channel();
    let control = AccountControl::start_with_clock_and_foreground(
        CredentialService,
        Arc::new(LoadProbe(loaded)),
        entry(),
        Arc::new(SystemClock),
        false,
        |_| {},
    );
    drop(control);
    assert!(matches!(
        loads.recv_timeout(Duration::from_secs(2)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
}

struct PausedRetry {
    called: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    first: bool,
}
impl AccountService for PausedRetry {
    fn execute(
        &mut self,
        generation: u64,
        _: AccountCommand,
        _: Option<&SessionCredential>,
        _: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        self.called.send(()).unwrap();
        if self.first {
            self.first = false;
            self.release.recv_timeout(Duration::from_secs(2)).unwrap();
            return Err(ServiceError::retryable("account_unavailable"));
        }
        Ok(ServiceOutcome {
            state: signed_out(generation),
            credential: CredentialChange::Keep,
            result: AccountOperationResult::SignedOut,
            follow_up: None,
        })
    }
}

#[test]
fn background_retains_inflight_failure_and_queued_retry_until_resume() {
    let (called, calls) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let (notify, events) = mpsc::channel();
    let control = AccountControl::start(
        PausedRetry {
            called,
            release: wait,
            first: true,
        },
        Arc::new(SessionCredentialStore::default()),
        entry(),
        move |event| {
            let _ = notify.send(event);
        },
    );
    control
        .enqueue(
            5,
            AccountCommand::StartGoogle {
                locale: "en".into(),
            },
        )
        .unwrap();
    calls.recv_timeout(Duration::from_secs(2)).unwrap();
    control.set_foreground(false);
    release.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while control.state().stage != AccountStage::Failed {
        events
            .recv_timeout(until.saturating_duration_since(Instant::now()))
            .unwrap();
    }
    let generation = control.enqueue(5, AccountCommand::Retry).unwrap();
    assert!(calls.recv_timeout(Duration::from_millis(30)).is_err());
    control.set_foreground(true);
    calls.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        terminal_result(&events, 5, generation),
        AccountOperationResult::SignedOut
    ));
    assert!(calls.recv_timeout(Duration::from_millis(30)).is_err());
}
