// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::account::directory::{AccountDirectory, MemoryAccountDirectory};
use crate::identity::AccountOwner;
use koru_oauth_credentials::SecretString;

const WAIT: Duration = Duration::from_secs(10);
#[test]
fn issued_access_cannot_be_paired_with_the_previous_durable_enrollment_owner() {
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(Hosted::new(false))),
        signed_descriptor(),
        "test".into(),
    )
    .with_directory_namespace(slot_entry().namespace());
    service.accept_tokens(tokens(session(2), "old")).unwrap();
    let record = crate::identity::DirectoryRecord {
        owner: owner(2),
        version: 1,
        state: crate::identity::DirectoryState::Ready,
    };
    service.credential_owner_acknowledged(Some(record));
    let before = service.enrollment_authorization().unwrap();
    assert_eq!(before.owner, owner(2));
    assert_eq!(before.access.0, "access-2");
    service.accept_tokens(tokens(session(3), "issued")).unwrap();
    assert!(service.enrollment_authorization().is_none());
    assert_eq!(service.durable_owner, Some(owner(2)));
    assert_eq!(
        service.unacknowledged_session_id,
        Some(session(3).to_string())
    );
}

fn owner(n: u8) -> AccountOwner {
    AccountOwner {
        account: format!("70000000-0000-4000-8000-{n:012}").parse().unwrap(),
        auth_identity: format!("80000000-0000-4000-8000-{n:012}").parse().unwrap(),
    }
}
fn session(n: u8) -> AccountSessionId {
    format!("90000000-0000-4000-8000-{n:012}").parse().unwrap()
}
fn account_for_session(id: AccountSessionId) -> u8 {
    if id == session(2) {
        2
    } else {
        assert!(id == session(1) || id == session(3));
        1
    }
}
fn slot_entry() -> CredentialEntry {
    CredentialContext::new(
        "test",
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/hosted-switching-fixture"),
        koru_device::DeviceId::parse(&signed_descriptor().device_id.to_string()).unwrap(),
    )
    .unwrap()
    .entry("https://account.invalid")
    .unwrap()
}
fn tokens(id: AccountSessionId, suffix: &str) -> TokenPairV1 {
    let account = account_for_session(id);
    TokenPairV1 {
        version: Version1,
        session_id: id,
        access_token: AccessToken(if id == session(3) {
            "issued-access-1".into()
        } else {
            format!("access-{account}")
        }),
        access_expires_at_millis: 2_000_000_000_000,
        refresh_token: koru_account_wire::RefreshToken(format!("refresh-{account}-{suffix}")),
        refresh_idle_expires_at_millis: 2_000_000_000_000,
        refresh_absolute_expires_at_millis: 2_000_000_000_000,
    }
}
fn summary(account: u8, n: u8) -> ProfileSummaryV1 {
    ProfileSummaryV1 {
        version: Version1,
        koru_identity_id: owner(account).account,
        profile_id: ProfileId::parse(&format!("a0000000-0000-4000-8000-{n:012}")).unwrap(),
        name_snapshot: Some(format!("Profile {n}")),
        locale_snapshot: None,
        auth_identities: vec![],
        lifecycle_revision: koru_account_core::ProfileLifecycleRevision::INITIAL,
    }
}
fn response(value: &impl serde::Serialize) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse {
        document_removal: false,
        status: 200,
        body: serde_json::to_vec(value).unwrap(),
    })
}
#[derive(Default)]
struct BarrierState {
    entered: usize,
    released: usize,
}
#[derive(Default)]
struct Barrier {
    state: Mutex<BarrierState>,
    changed: Condvar,
}
impl Barrier {
    fn hold(&self) {
        let mut state = self.state.lock().unwrap();
        state.entered += 1;
        let ticket = state.entered;
        self.changed.notify_all();
        while state.released < ticket {
            state = self.changed.wait(state).unwrap();
        }
    }
    fn entered(&self, count: usize) {
        let (state, _) = self
            .changed
            .wait_timeout_while(self.state.lock().unwrap(), WAIT, |state| {
                state.entered < count
            })
            .unwrap();
        assert!(
            state.entered >= count,
            "hosted effect must reach its controlled boundary"
        );
    }
    fn release(&self, count: usize) {
        self.state.lock().unwrap().released = count;
        self.changed.notify_all();
    }
}
struct Hosted {
    native: TokenTransport,
    attach: Arc<Barrier>,
    exchange: Arc<Barrier>,
    attachments: Mutex<Vec<(String, Vec<u8>)>>,
    exchanges: Mutex<Vec<Vec<u8>>>,
    uncertain_exchange: bool,
    repair_b: bool,
    logouts: Mutex<Vec<(String, LogoutRequestV1)>>,
}
impl Hosted {
    fn new(uncertain_exchange: bool) -> Self {
        Self {
            native: TokenTransport::default(),
            attach: Arc::new(Barrier::default()),
            exchange: Arc::new(Barrier::default()),
            attachments: Mutex::new(vec![]),
            exchanges: Mutex::new(vec![]),
            uncertain_exchange,
            repair_b: false,
            logouts: Mutex::new(vec![]),
        }
    }
}
impl Transport for Hosted {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if request.path_and_query == "/v1/oauth/native/session/inspect" {
            let inspect: koru_account_wire::NativeSessionInspectRequestV1 =
                serde_json::from_slice(&request.body).unwrap();
            let account = account_for_session(inspect.session_id);
            if self.repair_b && account == 2 {
                return Err(TransportError::Unavailable);
            }
            assert!(
                inspect
                    .refresh_token
                    .0
                    .starts_with(&format!("refresh-{account}-"))
            );
            return response(&SessionSummaryV1 {
                version: Version1,
                session_id: inspect.session_id,
                koru_identity_id: owner(account).account,
                auth_identity_id: owner(account).auth_identity,
                device_id: Some(inspect.device_id),
                expires_at_millis: 2_000_000_000_000,
                recent_auth_at_millis: 1,
            });
        }
        if request.path_and_query.ends_with("/refresh") {
            let refresh: RefreshRequestV1 = serde_json::from_slice(&request.body).unwrap();
            let account = account_for_session(refresh.session_id);
            assert!(
                refresh
                    .refresh_token
                    .0
                    .starts_with(&format!("refresh-{account}-"))
            );
            return response(&tokens(refresh.session_id, "rotated"));
        }
        if request.path_and_query.ends_with("/native/token/start")
            || request.path_and_query.ends_with("/native/token/complete")
        {
            // Only these two explicitly native paths reach the reused adapter.
            return self.native.send(request);
        }
        if request.path_and_query.ends_with("/native/device-challenge") {
            return Ok(device_challenge_response(&request));
        }
        if request.path_and_query.ends_with("/native/exchange") {
            let attempt = {
                let mut requests = self.exchanges.lock().unwrap();
                requests.push(request.body);
                requests.len()
            };
            self.exchange.hold();
            if self.uncertain_exchange && attempt == 1 {
                return Err(TransportError::Unavailable);
            }
            return response(&NativeExchangeResponseV1 {
                version: Version1,
                tokens: tokens(session(3), "issued"),
                account: AccountSummaryV1 {
                    version: Version1,
                    koru_identity_id: owner(1).account,
                    display_name: "A".into(),
                    auth_identities: vec![],
                    terms_version_at_creation: "test".into(),
                    created_at_millis: 1,
                },
            });
        }
        if request.path_and_query == "/v1/account/devices" {
            return response(&crate::account::tests::current_device_list());
        }
        let authorization = request
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.clone())
            .unwrap();
        let account = match authorization.as_str() {
            "Bearer access-1" | "Bearer issued-access-1" => 1,
            "Bearer access-2" => 2,
            _ => panic!("unexpected hosted bearer"),
        };
        if request.path_and_query.ends_with("/logout") {
            self.logouts.lock().unwrap().push((
                authorization,
                serde_json::from_slice(&request.body).unwrap(),
            ));
            return response(&koru_account_wire::EmptyResponseV1 { version: Version1 });
        }
        if request.path_and_query.ends_with("/profiles/attach") {
            let attach: AttachProfileRequestV1 = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(attach.profile_id, summary(1, 3).profile_id);
            self.attachments
                .lock()
                .unwrap()
                .push((authorization, request.body));
            self.attach.hold();
            return response(&summary(account, 3));
        }
        if request.path_and_query.ends_with("/profiles") {
            let profiles = if account == 1 {
                vec![summary(1, 1)]
            } else {
                vec![summary(2, 2), summary(2, 4)]
            };
            return response(&ProfileListResponseV1 {
                version: Version1,
                profiles,
            });
        }
        panic!(
            "unexpected hosted switching route: {}",
            request.path_and_query
        );
    }
}

struct Harness {
    control: AccountControl,
    events: mpsc::Receiver<WorkerEvent>,
    results: HashMap<u64, (u64, AccountOperationResult)>,
    hosted: Arc<Hosted>,
    delivery: Arc<TokenDelivery>,
    vault: Arc<SessionCredentialStore>,
    directory: Arc<MemoryAccountDirectory>,
}
impl Harness {
    fn new(uncertain_exchange: bool) -> Self {
        Self::with_repair(uncertain_exchange, false)
    }
    fn with_repair(uncertain_exchange: bool, repair_b: bool) -> Self {
        let mut hosted = Hosted::new(uncertain_exchange);
        hosted.repair_b = repair_b;
        let hosted = Arc::new(hosted);
        let delivery = Arc::new(TokenDelivery::default());
        let mut service = ProductionAccountService::with_client(
            AccountClient::with_transport(hosted.clone()),
            signed_descriptor(),
            "test".into(),
        )
        .with_directory_namespace(slot_entry().namespace());
        service.delivery = delivery.clone();
        let vault = Arc::new(SessionCredentialStore::default());
        let directory = Arc::new(MemoryAccountDirectory::new());
        let entry = slot_entry();
        directory.retire_legacy(&entry.namespace()).unwrap();
        for account in [1, 2] {
            let record = directory
                .pending(&entry.namespace(), owner(account))
                .unwrap();
            directory
                .ready(
                    &entry.namespace(),
                    &record,
                    record.owner,
                    account == 1,
                    false,
                )
                .unwrap();
            vault
                .store(
                    &entry
                        .for_account(&owner(account).account.to_string())
                        .unwrap(),
                    SessionCredential::new(
                        session(account).to_string(),
                        format!("refresh-{account}-seed"),
                    )
                    .unwrap()
                    .encode()
                    .unwrap(),
                )
                .unwrap();
        }
        let (tx, events) = mpsc::channel();
        let control = AccountControl::start_with_directory(
            service,
            vault.clone(),
            entry,
            Some(directory.clone()),
            Arc::new(SystemClock),
            true,
            move |event| {
                let _ = tx.send(event);
            },
        );
        let mut h = Self {
            control,
            events,
            results: HashMap::new(),
            hosted,
            delivery,
            vault,
            directory,
        };
        let deadline = Instant::now() + WAIT;
        while !h.control.state().held_accounts.iter().any(|a| {
            a.account_id == owner(1).account.to_string() && a.stage == AccountStage::SignedIn
        }) || !h.control.state().held_accounts.iter().any(|a| {
            a.account_id == owner(2).account.to_string()
                && a.stage
                    == if repair_b {
                        AccountStage::Failed
                    } else {
                        AccountStage::SignedIn
                    }
        }) {
            h.receive(deadline);
        }
        for account in if repair_b { vec![1] } else { vec![2, 1] } {
            h.switch(account);
            let generation = h.command(AccountCommand::ListProfiles);
            assert!(matches!(
                h.result(generation).1,
                AccountOperationResult::Profiles { .. }
            ));
        }
        h
    }
    fn command(&self, command: AccountCommand) -> u64 {
        self.control.enqueue(7, command).unwrap()
    }
    fn receive(&mut self, deadline: Instant) {
        if let WorkerEvent::Result {
            owner_scope,
            generation,
            result,
            ..
        } = self
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("owned worker event before deadline")
        {
            self.results.insert(generation, (owner_scope, result));
        }
    }
    fn result(&mut self, generation: u64) -> (u64, AccountOperationResult) {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(result) = self.results.remove(&generation) {
                return result;
            }
            self.receive(deadline);
        }
    }
    fn switch(&mut self, account: u8) -> u64 {
        let generation = self.command(AccountCommand::SwitchAccount {
            account_id: owner(account).account.to_string(),
        });
        let (scope, result) = self.result(generation);
        assert!(
            matches!(result, AccountOperationResult::AccountSelected { scope_id, account_id: Some(id) } if scope_id == scope && id == owner(account).account.to_string())
        );
        assert_eq!(
            self.control.state().active_account_id,
            Some(owner(account).account.to_string())
        );
        scope
    }
    fn bytes(&self, account: u8) -> SecretString {
        self.vault
            .load(
                &slot_entry()
                    .for_account(&owner(account).account.to_string())
                    .unwrap(),
            )
            .unwrap()
            .unwrap()
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.hosted.attach.release(usize::MAX);
        self.hosted.exchange.release(usize::MAX);
    }
}

#[test]
fn production_hosted_mutation_keeps_a_bearer_and_result_after_switching_to_b() {
    let mut h = Harness::new(false);
    let a_scope = h.control.state().active_scope;
    let a_before = h.bytes(1);
    let b_before = h.bytes(2);
    let generation = h.command(AccountCommand::AttachProfile {
        profile_id: summary(1, 3).profile_id.to_string(),
        name_snapshot: None,
        locale_snapshot: None,
    });
    h.hosted.attach.entered(1);
    let b_scope = h.switch(2);
    assert_eq!(h.control.state().profile_count, 2);
    let captured = h.hosted.attachments.lock().unwrap();
    assert_eq!(captured[0].0, "Bearer access-1");
    let request: AttachProfileRequestV1 = serde_json::from_slice(&captured[0].1).unwrap();
    assert_eq!(request.profile_id, summary(1, 3).profile_id);
    drop(captured);
    h.hosted.attach.release(1);
    let (scope, result) = h.result(generation);
    assert_eq!(scope, a_scope);
    assert!(
        matches!(result, AccountOperationResult::Profile { profile } if profile.koru_identity_id == owner(1).account.to_string() && profile.profile_id == summary(1, 3).profile_id.to_string())
    );
    assert_eq!(h.control.state().active_scope, b_scope);
    assert_eq!(h.bytes(1).expose(), a_before.expose());
    assert_eq!(h.bytes(2).expose(), b_before.expose());
    assert_eq!(h.switch(1), a_scope);
    assert_eq!(
        h.control.state().profile_count,
        2,
        "late mutation updated only A's profile cache"
    );
}

#[test]
fn production_uncertain_exchange_survives_switch_and_retries_exactly_without_second_provider() {
    let mut h = Harness::new(true);
    let a_scope = h.control.state().active_scope;
    let a_before = h.bytes(1);
    let b_before = h.bytes(2);
    let original = h.command(AccountCommand::StartGoogle {
        locale: "en".into(),
    });
    h.hosted.exchange.entered(1);
    let b_scope = h.switch(2);
    h.hosted.exchange.release(1);
    let deadline = Instant::now() + WAIT;
    while !h.control.state().held_accounts.iter().any(|a| {
        a.scope_id == a_scope
            && a.stage == AccountStage::Failed
            && a.failure_code.as_deref() == Some("account_unavailable")
    }) {
        h.receive(deadline);
    }
    assert!(
        h.control.requester_generation_active(7, original),
        "uncertain original remains owned for exact recovery"
    );
    assert!(
        !h.results.contains_key(&original),
        "retryable uncertainty is not a terminal result"
    );
    assert_eq!(h.control.state().active_scope, b_scope);
    assert_eq!(h.bytes(1).expose(), a_before.expose());
    assert_eq!(h.bytes(2).expose(), b_before.expose());
    assert_eq!(h.switch(1), a_scope);
    let retry = h.command(AccountCommand::Retry);
    h.hosted.exchange.entered(2);
    assert_eq!(h.switch(2), b_scope);
    let exchanges = h.hosted.exchanges.lock().unwrap();
    assert_eq!(exchanges.len(), 2);
    assert_eq!(
        exchanges[0], exchanges[1],
        "grant, state, verifier and signed device proof are retained byte-for-byte"
    );
    let exchange: NativeExchangeRequestV1 = serde_json::from_slice(&exchanges[1]).unwrap();
    assert!(exchange.device_ownership_proof.is_some());
    drop(exchanges);
    h.hosted.exchange.release(2);
    let (scope, result) = h.result(retry);
    assert_eq!(scope, a_scope);
    assert!(matches!(result, AccountOperationResult::SignedIn));
    assert_eq!(h.control.state().active_scope, b_scope);
    assert_eq!(h.bytes(2).expose(), b_before.expose());
    let a = SessionCredential::decode(&h.bytes(1)).unwrap();
    assert_eq!(a.session_id, session(3).to_string());
    assert_eq!(a.refresh_token, "refresh-1-issued");
    assert_eq!(h.delivery.prompts.lock().unwrap().len(), 1);
    assert_eq!(h.hosted.native.starts.load(Ordering::SeqCst), 1);
    assert_eq!(h.hosted.native.completes.lock().unwrap().len(), 1);
}

#[test]
fn production_repair_b_rejects_new_a_and_revokes_only_the_issued_session_before_any_vault_write() {
    let mut h = Harness::with_repair(false, true);
    let a_before = h.bytes(1);
    let b_before = h.bytes(2);
    let b_scope = h.switch(2);
    assert_eq!(h.control.state().stage, AccountStage::Failed);
    let before = h.directory.snapshot(&slot_entry().namespace()).unwrap();
    let generation = h.command(AccountCommand::StartGoogle {
        locale: "en".into(),
    });
    h.hosted.exchange.entered(1);
    assert_eq!(h.bytes(1).expose(), a_before.expose());
    assert_eq!(h.bytes(2).expose(), b_before.expose());
    h.hosted.exchange.release(1);
    let (scope, result) = h.result(generation);
    assert_eq!(scope, b_scope);
    assert!(
        matches!(result, AccountOperationResult::Failed { code } if code == "account_repair_identity_mismatch")
    );
    assert_eq!(h.bytes(1).expose(), a_before.expose());
    assert_eq!(h.bytes(2).expose(), b_before.expose());
    let after = h.directory.snapshot(&slot_entry().namespace()).unwrap();
    assert_eq!(
        after.records, before.records,
        "wrong owner is refused before directory pending-write allocation"
    );
    assert_eq!(after.legacy_retired, before.legacy_retired);
    let logouts = h.hosted.logouts.lock().unwrap();
    assert_eq!(logouts.len(), 1);
    assert_eq!(logouts[0].0, "Bearer issued-access-1");
    assert_eq!(logouts[0].1.session_id, session(3));
    assert_ne!(logouts[0].1.session_id, session(1));
    assert_ne!(logouts[0].1.session_id, session(2));
    assert_eq!(h.delivery.prompts.lock().unwrap().len(), 1);
    assert_eq!(h.hosted.exchanges.lock().unwrap().len(), 1);
}
