// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_account_runtime::account::{CredentialChange, ServiceOutcome, ServiceProgress};
use koru_oauth_credentials::{CredentialContext, SessionCredential};
use std::sync::atomic::{AtomicUsize, Ordering};
mod cloud_sync;
mod enrollment_sets;
mod production;
mod switching;

#[test]
fn poll_cannot_split_enqueue_from_requester_registration() {
    let bridge = PlatformBridge::new("trusted-web.apps.googleusercontent.com".into()).unwrap();
    let account = Arc::new(MobileAccount::start(
        FakeService(Arc::new(AtomicUsize::new(0))),
        bridge,
        entry(),
    ));
    // The born-backgrounded worker cannot consume the queued command.
    let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(0);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let command_account = account.clone();
    let command = std::thread::spawn(move || {
        command_account
            .command_with_admission(
                &wire::AccountCommandRequest {
                    expected_owner_scope: Some(1),
                    version: 1,
                    requester: 7,
                    action: wire::AccountAction::StartGoogle as i32,
                    locale: "en".into(),
                    ..Default::default()
                },
                |generation| {
                    admitted_tx.send(generation).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                },
            )
            .unwrap()
    });
    let generation = admitted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let active = account.control.requester_active(7);
    let admission_holds_poll_lock = account.results.try_lock().is_err();
    let poll_account = account.clone();
    let poll = std::thread::spawn(move || poll_account.poll(7, 0, false));
    release_tx.send(()).unwrap();
    assert_eq!(command.join().unwrap().generation, generation);
    assert!(active);
    assert!(
        admission_holds_poll_lock,
        "poll must not observe partially admitted ownership"
    );
    assert_eq!(
        poll.join().unwrap().status.unwrap().requester_active,
        Some(true)
    );
    assert_eq!(
        account.results.lock().unwrap().owners.get(&generation),
        Some(&7)
    );
}

#[test]
fn profile_choices_preserve_account_name_with_zero_profiles() {
    let result = operation_result(
        1,
        2,
        3,
        AccountOperationResult::ProfileChoices {
            display_name: "Existing account".into(),
            choices: vec![],
        },
    );
    assert_eq!(result.kind, "profiles");
    assert_eq!(result.account.unwrap().display_name, "Existing account");
    assert!(result.profiles.is_empty());
}

fn entry() -> CredentialEntry {
    CredentialContext::new(
        "test",
        std::path::Path::new("isolated-test-root"),
        koru_device::DeviceId::new(),
    )
    .unwrap()
    .entry("https://api.koru.do")
    .unwrap()
}
fn wait_for<T>(mut predicate: impl FnMut() -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(value) = predicate() {
            return value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "worker did not answer"
        );
        std::thread::yield_now();
    }
}
struct FakeService(Arc<AtomicUsize>);
impl AccountService for FakeService {
    fn execute(
        &mut self,
        _: u64,
        command: AccountCommand,
        _: Option<&SessionCredential>,
        _: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let logout = matches!(command, AccountCommand::Logout);
        Ok(ServiceOutcome {
            state: AccountState {
                capability: true,
                stage: if logout {
                    AccountStage::SignedOut
                } else {
                    AccountStage::SignedIn
                },
                ..Default::default()
            },
            credential: if logout {
                CredentialChange::Remove
            } else {
                CredentialChange::Replace(
                    SessionCredential::new("fixture-session".into(), "fixture-refresh".into())
                        .unwrap(),
                )
            },
            result: if logout {
                AccountOperationResult::SignedOut
            } else {
                AccountOperationResult::SignedIn
            },
            follow_up: None,
        })
    }
}
#[test]
fn platform_vault_ack_is_required_before_correlated_sign_in_and_logout() {
    let bridge = PlatformBridge::new("trusted-web.apps.googleusercontent.com".into()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let account = MobileAccount::start(FakeService(calls.clone()), bridge, entry());
    assert!(
        account.poll(7, 0, true).vault.is_none(),
        "born in background"
    );
    account.control.set_foreground(true);
    let load = wait_for(|| account.poll(7, 0, true).vault);
    assert_eq!(load.action, wire::AccountVaultAction::Load as i32);
    account
        .bridge
        .vault_reply(wire::AccountVaultReply {
            request_id: load.request_id,
            success: true,
            credential: None,
        })
        .unwrap();
    wait_for(|| (account.control.state().stage == AccountStage::SignedOut).then_some(()));
    let generation = account
        .command(&wire::AccountCommandRequest {
            expected_owner_scope: Some(account.control.state().active_scope),
            version: 1,
            requester: 7,
            action: wire::AccountAction::StartGoogle as i32,
            locale: "en".into(),
            ..Default::default()
        })
        .unwrap();
    let store = wait_for(|| account.poll(7, 0, true).vault);
    assert_eq!(store.action, wire::AccountVaultAction::Store as i32);
    assert_eq!(
        account.poll(7, 0, false).status.unwrap().requester_active,
        Some(true)
    );
    assert_eq!(
        account.poll(8, 0, false).status.unwrap().requester_active,
        Some(false)
    );
    assert!(account.poll(7, 0, false).vault.is_none());
    assert!(account.poll(7, 0, true).results.is_empty());
    assert!(
        account
            .bridge
            .vault_reply(wire::AccountVaultReply {
                request_id: store.request_id + 1,
                success: true,
                credential: None
            })
            .is_err()
    );
    account
        .bridge
        .vault_reply(wire::AccountVaultReply {
            request_id: store.request_id,
            success: true,
            credential: None,
        })
        .unwrap();
    let signed_in = wait_for(|| {
        account
            .poll(7, 0, true)
            .results
            .into_iter()
            .find(|v| v.kind == "signed_in")
    });
    assert_eq!(signed_in.generation, generation);
    assert!(account.poll(8, 0, true).results.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    account
        .command(&wire::AccountCommandRequest {
            expected_owner_scope: Some(account.control.state().active_scope),
            version: 1,
            requester: 7,
            action: wire::AccountAction::Logout as i32,
            ..Default::default()
        })
        .unwrap();
    let clear = wait_for(|| account.poll(7, 0, true).vault);
    assert_eq!(clear.action, wire::AccountVaultAction::Clear as i32);
    account
        .bridge
        .vault_reply(wire::AccountVaultReply {
            request_id: clear.request_id,
            success: true,
            credential: None,
        })
        .unwrap();
    wait_for(|| {
        account
            .poll(7, 0, true)
            .results
            .into_iter()
            .find(|v| v.kind == "signed_out")
    });
}
#[test]
fn native_reply_rejects_stale_and_duplicate_without_losing_pending_prompt() {
    let bridge = PlatformBridge::new("trusted-web.apps.googleusercontent.com".into()).unwrap();
    bridge.state.lock().unwrap().native = Some(NativePending {
        request: NativeSignInRequest {
            generation: 3,
            request_id: 4,
            nonce: "private-nonce".into(),
            expires_at_millis: i64::MAX,
            reauthentication: false,
        },
        response: None,
    });
    let reply = |generation| wire::NativeSignInReply {
        generation,
        request_id: 4,
        id_token: Some(b"private-token".to_vec()),
        cancelled: false,
    };
    assert_eq!(bridge.native_reply(reply(2)), Err("account_response_stale"));
    bridge.native_reply(reply(3)).unwrap();
    assert_eq!(bridge.native_reply(reply(3)), Err("account_response_stale"));
    assert!(
        bridge
            .state
            .lock()
            .unwrap()
            .native
            .as_ref()
            .unwrap()
            .response
            .is_some()
    );
}
#[test]
fn invalid_platform_secret_is_wiped_and_status_contains_no_secret() {
    let mut request = wire::AccountPlatformRequest {
        version: 1,
        requester: 1,
        response: Some(wire::account_platform_request::Response::SignIn(
            wire::NativeSignInReply {
                generation: 1,
                request_id: 1,
                id_token: Some(b"secret".to_vec()),
                cancelled: true,
            },
        )),
        ..Default::default()
    };
    assert!(koru_contract::mobile_account::platform(&request).is_err());
    zeroize_response(&mut request);
    if let Some(wire::account_platform_request::Response::SignIn(value)) = request.response {
        assert!(value.id_token.unwrap().iter().all(|v| *v == 0));
    }
    assert_eq!(status(AccountState::default()).enrollment_stage, "idle");
    assert_eq!(
        secret_bytes(vec![255]).err(),
        Some("account_payload_invalid")
    );
}
#[test]
fn result_cursor_gap_is_detectable() {
    let bridge = PlatformBridge::new("trusted-web.apps.googleusercontent.com".into()).unwrap();
    let account = MobileAccount::start(FakeService(Arc::new(AtomicUsize::new(0))), bridge, entry());
    account.results.lock().unwrap().evicted_through = 10;
    assert_eq!(
        account.poll(7, 9, false).error_code.as_deref(),
        Some("account_result_gap")
    );
    assert!(account.poll(7, 10, false).error_code.is_none());
}

struct AdvancingClock {
    base: std::time::Instant,
    elapsed: std::sync::atomic::AtomicU64,
}
impl koru_account_runtime::ports::Clock for AdvancingClock {
    fn now_millis(&self) -> Option<i64> {
        Some(self.elapsed.load(Ordering::SeqCst) as i64)
    }
    fn monotonic(&self) -> std::time::Instant {
        self.base + Duration::from_millis(self.elapsed.load(Ordering::SeqCst))
    }
}

#[test]
fn a_timed_out_vault_reply_cannot_acknowledge_a_later_operation() {
    let clock = Arc::new(AdvancingClock {
        base: std::time::Instant::now(),
        elapsed: std::sync::atomic::AtomicU64::new(0),
    });
    let bridge = PlatformBridge::with_clock(
        "trusted-web.apps.googleusercontent.com".into(),
        clock.clone(),
    )
    .unwrap();
    let task_bridge = bridge.clone();
    let task = std::thread::spawn(move || task_bridge.load(&entry()));
    let id = wait_for(|| bridge.state.lock().unwrap().vault.as_ref().map(|v| v.id));
    clock.elapsed.store(30_001, Ordering::SeqCst);
    bridge.changed.notify_all();
    assert!(matches!(task.join().unwrap(), Err(Error::Unavailable)));
    assert_eq!(
        bridge.vault_reply(wire::AccountVaultReply {
            request_id: id,
            success: true,
            credential: None
        }),
        Err("account_response_stale")
    );
    assert!(bridge.state.lock().unwrap().vault.is_none());
}

#[test]
fn phone_inventory_mapping_preserves_exact_account_and_removed_state() {
    use koru_account_runtime::account::lifecycle::{LocalProfileInventory, profile_availability};
    use koru_store_core::{
        ids::ProfileId, model::Timestamp, ports::ProfileLifecycleInventoryItem,
        profile_account::ProfileAccountBinding,
    };
    let account: koru_account_core::KoruIdentityId =
        "4ffb4f24-556b-46eb-9e8c-cf874925e03a".parse().unwrap();
    let profile = ProfileId::new();
    let rows = |removed| {
        inventory_rows(vec![ProfileLifecycleInventoryItem {
            profile,
            account_binding: ProfileAccountBinding::parse_identity_id(&account.to_string())
                .unwrap(),
            removed_at: removed,
        }])
    };
    assert_eq!(
        profile_availability(
            &rows(None),
            account,
            &[],
            koru_device::DeviceId::new(),
            profile
        ),
        (true, false)
    );
    assert_eq!(
        profile_availability(
            &rows(Some(Timestamp::from_millis(42))),
            account,
            &[],
            koru_device::DeviceId::new(),
            profile
        ),
        (false, false)
    );
    struct FakeInventory(koru_store_wire::ProfileLifecycleInventory);
    impl LocalProfileInventory for FakeInventory {
        fn inventory(&self) -> Result<koru_store_wire::ProfileLifecycleInventory, ServiceError> {
            Ok(self.0.clone())
        }
    }
    assert!(
        !FakeInventory(rows(Some(Timestamp::from_millis(42))))
            .authorize(profile, account)
            .unwrap()
    );
}

#[test]
fn native_wait_obeys_hosted_deadline_and_rejects_late_delivery() {
    struct WaitingService(Arc<PlatformBridge>);
    impl AccountService for WaitingService {
        fn execute(
            &mut self,
            generation: u64,
            _: AccountCommand,
            _: Option<&SessionCredential>,
            cancel: &Cancel,
            _: &mut dyn FnMut(ServiceProgress),
        ) -> Result<ServiceOutcome, ServiceError> {
            let mut delivery = NativeDelivery(self.0.clone());
            let _token = delivery.wait(
                &NativeSignInRequest {
                    generation,
                    request_id: 5,
                    nonce: "fixture-nonce".into(),
                    expires_at_millis: 500,
                    reauthentication: false,
                },
                cancel,
            )?;
            Err(ServiceError::terminal("unexpected_delivery"))
        }
    }
    let clock = Arc::new(AdvancingClock {
        base: std::time::Instant::now(),
        elapsed: std::sync::atomic::AtomicU64::new(0),
    });
    let bridge = PlatformBridge::with_clock(
        "trusted-web.apps.googleusercontent.com".into(),
        clock.clone(),
    )
    .unwrap();
    let account = MobileAccount::start(WaitingService(bridge.clone()), bridge, entry());
    account.control.set_foreground(true);
    let load = wait_for(|| account.poll(7, 0, true).vault);
    account
        .bridge
        .vault_reply(wire::AccountVaultReply {
            request_id: load.request_id,
            success: true,
            credential: None,
        })
        .unwrap();
    wait_for(|| (account.control.state().stage == AccountStage::SignedOut).then_some(()));
    let generation = account
        .command(&wire::AccountCommandRequest {
            expected_owner_scope: Some(account.control.state().active_scope),
            version: 1,
            requester: 7,
            action: wire::AccountAction::StartGoogle as i32,
            locale: "en".into(),
            ..Default::default()
        })
        .unwrap();
    wait_for(|| account.poll(7, 0, true).sign_in);
    clock.elapsed.store(501, Ordering::SeqCst);
    account.bridge.changed.notify_all();
    let failure = wait_for(|| {
        account
            .poll(7, 0, true)
            .results
            .into_iter()
            .find(|r| r.kind == "failed")
    });
    assert_eq!(
        failure.error_code.as_deref(),
        Some("native_sign_in_timeout")
    );
    assert_eq!(
        account.bridge.native_reply(wire::NativeSignInReply {
            generation,
            request_id: 5,
            id_token: Some(b"late-token".to_vec()),
            cancelled: false
        }),
        Err("account_response_stale")
    );
}

#[test]
fn a_browser_sign_in_returns_through_the_app_link_with_its_own_state_only() {
    let bridge = PlatformBridge::new("trusted-web.apps.googleusercontent.com".into()).unwrap();
    let delivery = NativeDelivery(Arc::clone(&bridge));
    let mut session = delivery.bind().unwrap();
    assert!(session.app_link_return());
    assert_eq!(session.callback_port(), 0);
    // A second browser sign-in cannot start while one is held.
    assert!(delivery.bind().is_err());

    bridge.browser_opened(7, "https://appleid.apple.com/auth/authorize".into());
    let request_id = bridge
        .state
        .lock()
        .unwrap()
        .browser
        .as_ref()
        .unwrap()
        .request_id;
    let reply = |request_id, url: &str| wire::BrowserSignInReply {
        generation: 7,
        request_id,
        callback_url: Some(url.into()),
        cancelled: false,
    };
    // Only the fixed HTTPS App Link path is a return.
    assert_eq!(
        bridge.browser_reply(reply(
            request_id,
            "http://www.koru.do/v1/oauth/app/callback?state=s"
        )),
        Err("account_payload_invalid")
    );
    assert_eq!(
        bridge.browser_reply(reply(request_id, "https://www.koru.do/elsewhere?state=s")),
        Err("account_payload_invalid")
    );
    assert_eq!(
        bridge.browser_reply(reply(
            request_id + 1,
            "https://www.koru.do/v1/oauth/app/callback?state=s"
        )),
        Err("account_response_stale")
    );

    let task_bridge = Arc::clone(&bridge);
    let waiter = std::thread::spawn(move || {
        let cancel = Cancel::never();
        let result = session
            .wait("leg-one", &cancel)
            .map(|callback| callback.values("outcome").next().map(str::to_owned));
        drop(session);
        (result, task_bridge)
    });
    // A link for another leg is dropped and the wait continues.
    bridge
        .browser_reply(reply(
            request_id,
            "https://www.koru.do/v1/oauth/app/callback?state=someone-else&outcome=completed",
        ))
        .unwrap();
    wait_for(|| {
        bridge
            .state
            .lock()
            .unwrap()
            .browser
            .as_ref()
            .filter(|p| p.reply.is_none())
            .map(|_| ())
    });
    bridge.browser_opened(7, "https://appleid.apple.com/auth/authorize".into());
    let request_id = bridge
        .state
        .lock()
        .unwrap()
        .browser
        .as_ref()
        .unwrap()
        .request_id;
    bridge
        .browser_reply(reply(
            request_id,
            "https://www.koru.do/v1/oauth/app/callback?state=leg-one&outcome=completed",
        ))
        .unwrap();
    let (result, bridge) = waiter.join().unwrap();
    assert_eq!(result.unwrap().as_deref(), Some("completed"));
    // The session released its slot when it ended.
    assert!(bridge.state.lock().unwrap().browser.is_none());
}

#[test]
fn inventory_result_echoes_exact_document_scope_on_success_and_failure() {
    use koru_agent_wire::{CloudInventoryFailure, CloudInventoryPayload};
    for failure in [
        None,
        Some(CloudInventoryFailure::RevisionChanged),
        Some(CloudInventoryFailure::Unavailable),
    ] {
        let result = operation_result(
            7,
            42,
            9,
            AccountOperationResult::CloudUsageDocuments {
                request_id: 42,
                workspace_id: "source-workspace".into(),
                incarnation_id: "source-incarnation".into(),
                payload: failure
                    .is_none()
                    .then(|| CloudInventoryPayload::new("{}".into()).unwrap()),
                failure,
            },
        );
        assert_eq!(result.kind, "cloud_usage_documents");
        assert_eq!(result.requester, 42);
        assert_eq!(result.generation, 9);
        assert_eq!(
            result.cloud_workspace_id.as_deref(),
            Some("source-workspace")
        );
        assert_eq!(
            result.cloud_incarnation_id.as_deref(),
            Some("source-incarnation")
        );
        assert_eq!(result.cloud_inventory_json.is_some(), failure.is_none());
        assert_eq!(result.error_code.as_deref(), failure.map(|f| f.code()));
    }
}
