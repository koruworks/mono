// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::delivery::Callback;
use koru_account_wire::{
    AccountCreationRequestV1, CollisionPromptV1, GoogleCallbackResponseV1, NativeLoopbackReturnV1,
};

const COLLISION_STATE: &str = "fixture-fresh-proof-state";
const COLLISION_URL: &str = "https://accounts.google.com/fixture-fresh-proof";
const COLLISION_FLOW: &str = "60000000-0000-4000-8000-000000000006";

#[derive(Default)]
struct BrowserProbe {
    binds: AtomicUsize,
    waits: Mutex<Vec<String>>,
    retry_first_collision_wait: AtomicBool,
    cancel_collision_wait: AtomicBool,
    initial_collision: bool,
}
struct BrowserDelivery(Arc<BrowserProbe>);
impl SignInDelivery for BrowserDelivery {
    fn bind(&self) -> Result<Box<dyn SignInSession>, ServiceError> {
        self.0.binds.fetch_add(1, Ordering::AcqRel);
        Ok(Box::new(BrowserSession(self.0.clone())))
    }
}
struct BrowserSession(Arc<BrowserProbe>);
impl SignInSession for BrowserSession {
    fn callback_port(&self) -> u16 {
        43123
    }
    fn wait(&mut self, state: &str, cancel: &Cancel) -> Result<Callback, ServiceError> {
        let first = self.0.waits.lock().unwrap().is_empty();
        self.0.waits.lock().unwrap().push(state.into());
        if first {
            let mut fields = vec![
                ("state".into(), state.into()),
                (
                    "expires_at_millis".into(),
                    (CREATION_NOW + 300_000).to_string(),
                ),
            ];
            if self.0.initial_collision {
                fields.extend([
                    ("outcome".into(), "collision_required".into()),
                    (
                        "collision_flow_id".into(),
                        "70000000-0000-4000-8000-000000000007".into(),
                    ),
                    ("reauthentication_flow_id".into(), COLLISION_FLOW.into()),
                    ("reauthentication_state".into(), COLLISION_STATE.into()),
                    ("authorization_url".into(), COLLISION_URL.into()),
                ]);
            } else {
                fields.extend([
                    ("outcome".into(), "confirmation_required".into()),
                    (
                        "confirmation".into(),
                        "fixture-confirmation-authority".into(),
                    ),
                ]);
            }
            return Ok(Callback::new(fields));
        }
        assert_eq!(state, COLLISION_STATE);
        if self.0.cancel_collision_wait.load(Ordering::Acquire) {
            cancel.0.store(true, Ordering::Release);
            return Err(ServiceError::terminal("cancelled"));
        }
        if self
            .0
            .retry_first_collision_wait
            .swap(false, Ordering::AcqRel)
        {
            return Err(ServiceError::retryable("fixture_listener_retry"));
        }
        Ok(Callback::new(vec![
            ("state".into(), state.into()),
            ("outcome".into(), "completed".into()),
            ("return_target".into(), "account".into()),
            (
                "grant".into(),
                "50000000-0000-4000-8000-000000000005.one-use".into(),
            ),
            (
                "expires_at_millis".into(),
                (CREATION_NOW + 300_000).to_string(),
            ),
        ]))
    }
}

#[derive(Default)]
struct BrowserTransport {
    creation_without_collision: bool,
    starts: Mutex<Vec<GoogleNativeStartRequestV1>>,
    decisions: Mutex<Vec<(String, Vec<u8>)>>,
    exchanges: Mutex<Vec<NativeExchangeRequestV1>>,
}
impl Transport for BrowserTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let response = if request.path_and_query.ends_with("/google/native/start") {
            let start: GoogleNativeStartRequestV1 = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(start.callback_port, 43123);
            self.starts.lock().unwrap().push(start);
            serde_json::to_vec(&AuthStartResponseV1 {
                version: Version1,
                flow_id: native_start(false).flow_id,
                authorization_url: "https://accounts.google.com/fixture-initial".into(),
                expires_at_millis: CREATION_NOW + 300_000,
            })
            .unwrap()
        } else if request.path_and_query.ends_with("/account/confirm") {
            let decision: AccountCreationRequestV1 = serde_json::from_slice(&request.body).unwrap();
            self.decisions
                .lock()
                .unwrap()
                .push((request.path_and_query, request.body));
            serde_json::to_vec(&AccountCreationResponseV1::Browser {
                callback: if self.creation_without_collision {
                    GoogleCallbackResponseV1::Completed {
                        version: Version1,
                        grant: OneUseGrant("50000000-0000-4000-8000-000000000005.one-use".into()),
                        return_target: koru_account_wire::SymbolicReturnTarget::Account,
                        expires_at_millis: CREATION_NOW + 300_000,
                        web_return: None,
                        native_return: Some(NativeLoopbackReturnV1 {
                            port: 43123,
                            client_state: decision.client_state,
                            app_link: false,
                        }),
                    }
                } else {
                    GoogleCallbackResponseV1::CollisionRequired {
                        version: Version1,
                        collision_flow_id: "70000000-0000-4000-8000-000000000007".parse().unwrap(),
                        reauthentication: Box::new(AuthStartResponseV1 {
                            version: Version1,
                            flow_id: COLLISION_FLOW.parse().unwrap(),
                            authorization_url: COLLISION_URL.into(),
                            expires_at_millis: CREATION_NOW + 300_000,
                        }),
                        provider: koru_account_core::AuthProvider::Google,
                        prompt: CollisionPromptV1::SelectAccount,
                        web_return: None,
                        native_return: Some(NativeLoopbackReturnV1 {
                            port: 43123,
                            client_state: decision.client_state,
                            app_link: false,
                        }),
                        reauthentication_web_return: None,
                        reauthentication_native_return: Some(NativeLoopbackReturnV1 {
                            port: 43123,
                            client_state: OAuthState(COLLISION_STATE.into()),
                            app_link: false,
                        }),
                    }
                },
            })
            .unwrap()
        } else if request.path_and_query.ends_with("/native/exchange") {
            self.exchanges
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&request.body).unwrap());
            return NativeFlowTransport.send(request);
        } else if request.path_and_query.ends_with("/native/device-challenge") {
            return Ok(device_challenge_response(&request));
        } else {
            panic!("unexpected fake browser route");
        };
        Ok(HttpResponse {
            document_removal: false,
            status: 200,
            body: response,
        })
    }
}
fn browser_service(
    transport: Arc<BrowserTransport>,
    probe: Arc<BrowserProbe>,
) -> ProductionAccountService {
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(transport),
        signed_descriptor(),
        "test".into(),
    );
    service.delivery = Arc::new(BrowserDelivery(probe));
    service.clock = Arc::new(CreationClock(Instant::now()));
    service.entropy = Arc::new(CreationEntropy);
    service
}
fn browser_execute(
    service: &mut ProductionAccountService,
    command: AccountCommand,
    urls: &mut Vec<String>,
) -> Result<ServiceOutcome, ServiceError> {
    service.execute(
        8,
        command,
        None,
        &Cancel(Arc::new(AtomicBool::new(false))),
        &mut |event| {
            if let ServiceProgress::Browser(url) = event {
                urls.push(url.0);
            }
        },
    )
}

#[test]
fn browser_creates_unseen_account_and_signs_in_without_another_user_command() {
    let transport = Arc::new(BrowserTransport {
        creation_without_collision: true,
        ..Default::default()
    });
    let probe = Arc::new(BrowserProbe::default());
    let mut service = browser_service(transport.clone(), probe.clone());
    let mut urls = Vec::new();
    let result = browser_execute(
        &mut service,
        AccountCommand::StartGoogle {
            locale: "en".into(),
            ab: None,
        },
        &mut urls,
    )
    .unwrap();
    assert_eq!(result.state.stage, AccountStage::SignedIn);
    assert!(matches!(result.result, AccountOperationResult::SignedIn));
    assert!(matches!(result.credential, CredentialChange::Replace(_)));
    assert_eq!(urls, vec!["https://accounts.google.com/fixture-initial"]);
    assert_eq!(probe.binds.load(Ordering::Acquire), 1);
    assert_eq!(probe.waits.lock().unwrap().len(), 1);
    assert_eq!(transport.decisions.lock().unwrap().len(), 1);
    assert_eq!(transport.exchanges.lock().unwrap().len(), 1);
    assert!(!service.cancellation_safe());
    service.credential_acknowledged();
    assert!(service.pending_creation.is_none());
}

#[test]
fn creation_browser_collision_retry_reuses_listener_state_proof_and_opens_each_url_once() {
    let transport = Arc::new(BrowserTransport::default());
    let probe = Arc::new(BrowserProbe::default());
    probe
        .retry_first_collision_wait
        .store(true, Ordering::Release);
    let mut service = browser_service(transport.clone(), probe.clone());
    let mut urls = Vec::new();
    let error = browser_execute(
        &mut service,
        AccountCommand::StartGoogle {
            locale: "en".into(),
            ab: None,
        },
        &mut urls,
    )
    .err()
    .unwrap();
    assert!(error.retryable);
    assert!(transport.exchanges.lock().unwrap().is_empty());
    let result = browser_execute(&mut service, AccountCommand::Retry, &mut urls).unwrap();
    assert!(matches!(result.result, AccountOperationResult::SignedIn));
    assert_eq!(
        urls,
        vec!["https://accounts.google.com/fixture-initial", COLLISION_URL]
    );
    assert_eq!(probe.binds.load(Ordering::Acquire), 1);
    let waits = probe.waits.lock().unwrap();
    assert_eq!(&waits[1..], &[COLLISION_STATE, COLLISION_STATE]);
    let decisions = transport.decisions.lock().unwrap();
    assert_eq!(decisions.len(), 2);
    assert_eq!(decisions[0], decisions[1]);
    let decision: AccountCreationRequestV1 = serde_json::from_slice(&decisions[0].1).unwrap();
    assert_eq!(decision.client_state.0, waits[0]);
    let exchanges = transport.exchanges.lock().unwrap();
    assert_eq!(exchanges.len(), 1);
    assert_eq!(exchanges[0].flow_id.to_string(), COLLISION_FLOW);
    assert_eq!(exchanges[0].client_state, decision.client_state);
    assert_eq!(
        Some(exchanges[0].pkce_verifier.clone()),
        decision.pkce_verifier
    );
    assert_eq!(
        transport.starts.lock().unwrap()[0].pkce_challenge,
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(exchanges[0].pkce_verifier.0.as_bytes()))
    );
    assert!(service.pending_creation.is_some());
    drop(exchanges);
    service.credential_acknowledged();
    assert!(service.pending_creation.is_none());
}

#[test]
fn cancellation_during_known_browser_collision_wait_releases_creation_without_exchange_or_reopen() {
    let transport = Arc::new(BrowserTransport::default());
    let probe = Arc::new(BrowserProbe::default());
    probe.cancel_collision_wait.store(true, Ordering::Release);
    let mut service = browser_service(transport.clone(), probe.clone());
    let mut urls = Vec::new();
    let result = browser_execute(
        &mut service,
        AccountCommand::StartGoogle {
            locale: "en".into(),
            ab: None,
        },
        &mut urls,
    )
    .unwrap();
    assert!(matches!(result.result, AccountOperationResult::Cancelled));
    assert!(service.pending_creation.is_none());
    assert!(service.cancellation_safe());
    assert!(transport.exchanges.lock().unwrap().is_empty());
    assert_eq!(probe.binds.load(Ordering::Acquire), 1);
    assert_eq!(
        urls.iter()
            .filter(|url| url.as_str() == COLLISION_URL)
            .count(),
        1
    );
    assert!(decide(&mut service, AccountCommand::ConfirmAccountCreation, false).is_err());
    assert_eq!(transport.decisions.lock().unwrap().len(), 1);
}

#[test]
fn ordinary_browser_collision_authenticates_existing_account_without_a_creation_decision() {
    let transport = Arc::new(BrowserTransport::default());
    let probe = Arc::new(BrowserProbe {
        initial_collision: true,
        ..Default::default()
    });
    let mut service = browser_service(transport.clone(), probe.clone());
    let mut urls = Vec::new();
    let result = browser_execute(
        &mut service,
        AccountCommand::StartGoogle {
            locale: "en".into(),
            ab: None,
        },
        &mut urls,
    )
    .unwrap();
    assert!(matches!(result.result, AccountOperationResult::SignedIn));
    assert!(service.pending_creation.is_none());
    assert!(transport.decisions.lock().unwrap().is_empty());
    assert_eq!(transport.exchanges.lock().unwrap().len(), 1);
    assert_eq!(probe.binds.load(Ordering::Acquire), 1);
    assert_eq!(probe.waits.lock().unwrap().len(), 2);
}

#[test]
fn control_generic_cancel_after_lost_confirm_recovers_exact_decision_exchanges_and_revokes() {
    assert_control_cancel_recovers(0, false);
}

#[test]
fn control_cancel_recovers_malformed_or_timed_out_confirmation_and_exchange() {
    for response in [1, 2] {
        for exchange in [false, true] {
            assert_control_cancel_recovers(response, exchange);
        }
    }
}

fn assert_control_cancel_recovers(response: usize, exchange: bool) {
    let (entered, committed) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let transport = Arc::new(CreationTransport {
        first_decision_gate: Some((entered, Mutex::new(blocked))),
        fault_exchange: exchange,
        ..Default::default()
    });
    transport
        .lose_decision
        .store(usize::from(response == 0), Ordering::Release);
    transport
        .uncertain_response
        .store(response, Ordering::Release);
    let delivery = Arc::new(TokenDelivery::default());
    let store = Arc::new(SessionCredentialStore::default());
    let (notify, events) = mpsc::channel();
    let control = AccountControl::start_with_clock(
        creation_service(transport.clone(), delivery.clone()),
        store.clone(),
        entry(),
        Arc::new(CreationClock(Instant::now())),
        move |event| {
            let _ = notify.send(event);
        },
    );
    let start = control
        .enqueue(
            9,
            AccountCommand::StartGoogle {
                locale: "en".into(),
                ab: None,
            },
        )
        .unwrap();
    committed.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(*transport.committed_decision.lock().unwrap(), Some(true));
    control
        .enqueue(9, AccountCommand::Cancel { generation: start })
        .unwrap();
    release.send(()).unwrap();
    assert!(matches!(
        terminal_result(&events, 9, start),
        AccountOperationResult::SignedOut | AccountOperationResult::Cancelled
    ));
    assert_eq!(control.state().stage, AccountStage::SignedOut);
    assert!(store.load(&entry()).unwrap().is_none());
    let decisions = transport.decisions.lock().unwrap();
    assert_eq!(decisions.len(), if exchange { 1 } else { 2 });
    assert!(decisions.iter().all(|request| request == &decisions[0]));
    assert!(decisions[0].0.ends_with("/confirm"));
    let exchanges = transport.base.exchanges.lock().unwrap();
    // Cleanup may resolve the exchange more than once before the vault ACK;
    // every attempt must remain the exact same server idempotency request.
    assert!(exchanges.len() >= if exchange { 2 } else { 1 });
    assert!(exchanges.iter().all(|request| request == &exchanges[0]));
    assert_eq!(transport.logouts.load(Ordering::Acquire), 1);
    assert_eq!(delivery.prompts.lock().unwrap().len(), 1);
}

fn assert_creation_expired(result: &AccountOperationResult) {
    assert!(
        matches!(result, AccountOperationResult::Failed { code } if code == "account_confirmation_expired")
    );
}

fn wait_failure(control: &AccountControl, events: &mpsc::Receiver<WorkerEvent>, code: &str) {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let event = events
            .recv_timeout(until.saturating_duration_since(Instant::now()))
            .expect("worker publishes the armed failure");
        if matches!(event, WorkerEvent::Changed)
            && control.state().failure_code.as_deref() == Some(code)
        {
            return;
        }
    }
}

#[test]
fn creation_challenge_and_uncertain_exchange_expiry_clear_proof_but_keep_existing_credential() {
    for challenge in [false, true] {
        for existing in [false, true] {
            let transport = Arc::new(CreationTransport::default());
            let delivery = Arc::new(TokenDelivery::default());
            let mut service = creation_service(transport.clone(), delivery.clone());
            if existing {
                service
                    .accept_tokens(seeded_tokens("previous-refresh"))
                    .unwrap();
            }
            let old_access = service.access.clone();
            let result = if challenge {
                transport.expired_challenge.store(true, Ordering::Release);
                execute_native(&mut service).unwrap()
            } else {
                transport.base.lose_exchange.store(1, Ordering::Release);
                assert!(execute_native(&mut service).err().unwrap().retryable);
                transport.expired_exchange.store(true, Ordering::Release);
                decide(&mut service, AccountCommand::Retry, false).unwrap()
            };
            assert_creation_expired(&result.result);
            assert!(matches!(result.credential, CredentialChange::Keep));
            assert_eq!(service.access, old_access);
            assert_eq!(
                result.state.stage,
                if existing {
                    AccountStage::SignedIn
                } else {
                    AccountStage::SignedOut
                }
            );
            assert!(service.pending_creation.is_none());
            assert!(service.pending_native.is_none());
            assert!(service.pending_exchange.is_none());
            assert!(service.cancellation_safe());
            assert_eq!(transport.logouts.load(Ordering::Acquire), 0);
            assert_eq!(transport.decisions.lock().unwrap().len(), 1);
            assert_eq!(delivery.prompts.lock().unwrap().len(), 1);
            transport.expired_challenge.store(false, Ordering::Release);
            transport.expired_exchange.store(false, Ordering::Release);
            assert_eq!(
                execute_native(&mut service).unwrap().state.stage,
                AccountStage::SignedIn
            );
            assert_eq!(
                delivery.prompts.lock().unwrap().len(),
                2,
                "expiry requires an explicit fresh sign-in"
            );
        }
    }
}

#[test]
fn control_cancel_and_logout_after_lost_exchange_report_expiry_without_claiming_revocation() {
    for logout in [false, true] {
        let transport = Arc::new(CreationTransport::default());
        transport.base.lose_exchange.store(1, Ordering::Release);
        let delivery = Arc::new(TokenDelivery::default());
        let store = Arc::new(SessionCredentialStore::default());
        let (notify, events) = mpsc::channel();
        let control = AccountControl::start_with_clock(
            creation_service(transport.clone(), delivery.clone()),
            store.clone(),
            entry(),
            Arc::new(CreationClock(Instant::now())),
            move |event| {
                let _ = notify.send(event);
            },
        );
        let start = control
            .enqueue(
                19,
                AccountCommand::StartGoogle {
                    locale: "en".into(),
                    ab: None,
                },
            )
            .unwrap();
        wait_failure(&control, &events, "account_unavailable");
        transport.expired_exchange.store(true, Ordering::Release);
        let command = if logout {
            AccountCommand::Logout
        } else {
            AccountCommand::Cancel { generation: start }
        };
        let generation = control.enqueue(19, command).unwrap();
        assert_creation_expired(&terminal_result(&events, 19, generation));
        assert_eq!(
            control.state().failure_code.as_deref(),
            Some("account_confirmation_expired")
        );
        assert!(store.load(&entry()).unwrap().is_none());
        assert_eq!(transport.logouts.load(Ordering::Acquire), 0);
        assert_eq!(delivery.prompts.lock().unwrap().len(), 1);
    }
}

#[test]
fn creation_known_issued_session_logout_uses_new_id_even_with_expired_grant_and_old_restored_id() {
    for returning in [false, true] {
        let transport = Arc::new(CreationTransport {
            returning,
            ..Default::default()
        });
        let mut service = creation_service(transport.clone(), Arc::new(TokenDelivery::default()));
        let mut old_tokens = seeded_tokens("old-refresh");
        old_tokens.session_id = "80000000-0000-4000-8000-000000000008".parse().unwrap();
        old_tokens.access_token = AccessToken("older-session-access".into());
        let old = service.accept_tokens(old_tokens).unwrap();
        service.credential_acknowledged();
        let started = execute_native(&mut service).unwrap();
        let CredentialChange::Replace(issued) = started.credential else {
            panic!()
        };
        assert_ne!(issued.session_id, old.session_id);
        let exchanges = transport.base.exchanges.lock().unwrap().len();
        transport.expired_exchange.store(true, Ordering::Release);
        let result = service
            .execute(
                9,
                AccountCommand::Logout,
                Some(&old),
                &Cancel(Arc::new(AtomicBool::new(false))),
                &mut |_| {},
            )
            .unwrap();
        assert!(matches!(result.result, AccountOperationResult::SignedOut));
        assert_eq!(transport.base.exchanges.lock().unwrap().len(), exchanges);
        let logouts = transport.logout_requests.lock().unwrap();
        assert_eq!(logouts.len(), 1);
        assert_eq!(logouts[0].session_id.to_string(), issued.session_id);
        assert_eq!(
            transport.logout_authorizations.lock().unwrap().as_slice(),
            &[Some("Bearer access-memory-only".into())]
        );
        assert!(service.pending_creation.is_none());
    }
}

#[test]
fn control_cancel_after_vault_failure_revokes_known_session_without_replaying_expired_grant() {
    let transport = Arc::new(CreationTransport::default());
    let delivery = Arc::new(TokenDelivery::default());
    let store = Arc::new(FailOnceStore {
        failed: AtomicBool::new(false),
        inner: SessionCredentialStore::default(),
    });
    let (notify, events) = mpsc::channel();
    let control = AccountControl::start_with_clock(
        creation_service(transport.clone(), delivery.clone()),
        store.clone(),
        entry(),
        Arc::new(CreationClock(Instant::now())),
        move |event| {
            let _ = notify.send(event);
        },
    );
    let start = control
        .enqueue(
            29,
            AccountCommand::StartGoogle {
                locale: "en".into(),
                ab: None,
            },
        )
        .unwrap();
    wait_failure(&control, &events, "credential_unavailable");
    assert!(store.load(&entry()).unwrap().is_none());
    let exchanges = transport.base.exchanges.lock().unwrap().len();
    assert_eq!(exchanges, 1);
    transport.expired_exchange.store(true, Ordering::Release);
    control
        .enqueue(29, AccountCommand::Cancel { generation: start })
        .unwrap();
    assert!(matches!(
        terminal_result(&events, 29, start),
        AccountOperationResult::SignedOut | AccountOperationResult::Cancelled
    ));
    assert_eq!(control.state().stage, AccountStage::SignedOut);
    assert!(store.load(&entry()).unwrap().is_none());
    assert_eq!(transport.base.exchanges.lock().unwrap().len(), exchanges);
    let logouts = transport.logout_requests.lock().unwrap();
    assert_eq!(logouts.len(), 1);
    assert_eq!(logouts[0].session_id, seeded_tokens("unused").session_id);
    assert_eq!(delivery.prompts.lock().unwrap().len(), 1);
}
