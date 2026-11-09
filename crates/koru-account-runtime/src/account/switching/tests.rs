// SPDX-License-Identifier: MPL-2.0

use super::*;
mod refresh;
use crate::account::directory::{
    AccountDirectory, CredentialPersistence, MemoryAccountDirectory, PendingCredentialWrite,
};
use crate::enrollment::{EnrollmentAuthorization, ResponderSessionGate};
use crate::identity::{AccountOwner, DirectoryRecord};
use koru_oauth_credentials::{
    CredentialContext, Error as VaultError, SecretString, SessionCredentialStore,
};

const WAIT: Duration = Duration::from_secs(10);

fn owner(account: u8) -> AccountOwner {
    AccountOwner {
        account: format!("10000000-0000-4000-8000-{account:012}")
            .parse()
            .unwrap(),
        auth_identity: format!("20000000-0000-4000-8000-{account:012}")
            .parse()
            .unwrap(),
    }
}
fn entry() -> CredentialEntry {
    CredentialContext::new(
        "nightly",
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/switching-fixture"),
        koru_device::DeviceId::parse("30000000-0000-4000-8000-000000000001").unwrap(),
    )
    .unwrap()
    .entry("https://account.invalid")
    .unwrap()
}
fn credential(account: u8, revision: usize) -> SessionCredential {
    SessionCredential::new(
        format!("session-{account}"),
        format!("refresh-{account}-{revision}"),
    )
    .unwrap()
}
fn account_of(credential: &SessionCredential) -> u8 {
    credential
        .session_id
        .strip_prefix("session-")
        .unwrap()
        .split('-')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Boundary {
    Inspect,
    InspectLegacy,
    Execute,
    Vault,
}
#[derive(Default)]
struct GateState {
    armed: Option<Boundary>,
    entered: bool,
    released: bool,
}
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}
impl Gate {
    fn arm(&self, boundary: Boundary) {
        *self.state.lock().unwrap() = GateState {
            armed: Some(boundary),
            entered: false,
            released: false,
        };
    }
    fn cross(&self, boundary: Boundary, account: u8) {
        let mut state = self.state.lock().unwrap();
        if account != 1 || state.armed != Some(boundary) {
            return;
        }
        state.armed = None;
        state.entered = true;
        self.changed.notify_all();
        while !state.released {
            state = self.changed.wait(state).unwrap();
        }
    }
    fn entered(&self) {
        let state = self.state.lock().unwrap();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, WAIT, |s| !s.entered)
            .unwrap();
        assert!(state.entered, "effect must reach its injected barrier");
    }
    fn release(&self) {
        self.state.lock().unwrap().released = true;
        self.changed.notify_all();
    }
}
struct Vault {
    memory: SessionCredentialStore,
    gate: Arc<Gate>,
}
impl CredentialStore for Vault {
    fn load(&self, entry: &CredentialEntry) -> Result<Option<SecretString>, VaultError> {
        self.memory.load(entry)
    }
    fn store(&self, entry: &CredentialEntry, secret: SecretString) -> Result<(), VaultError> {
        self.gate.cross(
            Boundary::Vault,
            account_of(&SessionCredential::decode(&secret)?),
        );
        self.memory.store(entry, secret)
    }
    fn clear(&self, entry: &CredentialEntry) -> Result<(), VaultError> {
        self.memory.clear(entry)
    }
}
#[derive(Default)]
struct Probe {
    gate: Arc<Gate>,
    next_service: AtomicUsize,
    rotations: AtomicUsize,
    executions: Mutex<Vec<(usize, u8)>>,
    fail_list_once: AtomicBool,
    signin_account: AtomicUsize,
    logout_sessions: Mutex<Vec<String>>,
    acknowledged_records: Mutex<Vec<DirectoryRecord>>,
    refresh_deadlines: AtomicBool,
    now_millis: std::sync::atomic::AtomicI64,
    fail_refresh_once: AtomicBool,
    refresh_unavailable_once: AtomicBool,
    list_calls: AtomicUsize,
    rejected_refresh: Mutex<Option<String>>,
    /// The refresh token whose *refresh* is answered as revoked. Distinct from
    /// `rejected_refresh`, which only refuses the inspection probe: the two
    /// together are a genuinely dead session, and `rejected_refresh` alone is
    /// a rotation that was answered but never reached the vault.
    revoked_refresh: Mutex<Option<String>>,
}
struct Service {
    probe: Arc<Probe>,
    id: usize,
    record: Option<DirectoryRecord>,
    session: Option<String>,
    refresh_at: Option<i64>,
}
impl Service {
    fn new(probe: Arc<Probe>) -> Self {
        let id = probe.next_service.fetch_add(1, Ordering::Relaxed);
        Self {
            probe,
            id,
            record: None,
            session: None,
            refresh_at: None,
        }
    }
}
impl AccountService for Service {
    fn access_refresh_at_millis(&self) -> Option<i64> {
        self.refresh_at
    }
    fn fork(&self) -> Option<Box<dyn AccountService>> {
        Some(Box::new(Self::new(self.probe.clone())))
    }
    fn inspect_credential(
        &mut self,
        credential: &SessionCredential,
    ) -> Result<AccountOwner, ServiceError> {
        let account = account_of(credential);
        if self.probe.rejected_refresh.lock().unwrap().as_deref()
            == Some(credential.refresh_token.as_str())
        {
            return Err(ServiceError::terminal("credential_rejected"));
        }
        if credential.refresh_token == "legacy" {
            self.probe.gate.cross(Boundary::InspectLegacy, 1);
        }
        self.probe.gate.cross(Boundary::Inspect, account);
        Ok(owner(account))
    }
    fn credential_owner_acknowledged(&mut self, record: Option<DirectoryRecord>) {
        if let Some(record) = &record {
            self.probe
                .acknowledged_records
                .lock()
                .unwrap()
                .push(record.clone());
        }
        self.record = record;
    }
    fn credential_removed(&mut self, _: Option<AccountOwner>) {
        self.record = None;
    }
    fn enrollment_authority(&self) -> Option<EnrollmentAuthorization> {
        let record = self.record.as_ref()?;
        Some(EnrollmentAuthorization {
            access: AccessToken("injected-access".into()),
            owner: record.owner,
            namespace: entry().namespace(),
            record_version: record.version,
            session_id: self.session.clone()?,
        })
    }
    fn execute(
        &mut self,
        generation: u64,
        command: AccountCommand,
        restored: Option<&SessionCredential>,
        _: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        let fallback = credential(1, 0);
        let restored = restored.unwrap_or_else(|| {
            assert!(matches!(command, AccountCommand::StartGoogle { .. }));
            &fallback
        });
        let account = account_of(restored);
        if matches!(command, AccountCommand::Retry)
            && self.probe.revoked_refresh.lock().unwrap().as_deref()
                == Some(restored.refresh_token.as_str())
        {
            return Err(ServiceError::revoked());
        }
        if matches!(command, AccountCommand::Retry)
            && self.probe.refresh_deadlines.load(Ordering::Acquire)
        {
            if account == 1 && self.probe.fail_refresh_once.swap(false, Ordering::AcqRel) {
                return Err(ServiceError::terminal("refresh_fixture_refused"));
            }
            if account == 1
                && self
                    .probe
                    .refresh_unavailable_once
                    .swap(false, Ordering::AcqRel)
            {
                return Err(ServiceError::retryable("account_unavailable"));
            }
            self.refresh_at = Some(self.probe.now_millis.load(Ordering::Acquire) + 60_000);
        }
        self.session = Some(restored.session_id.clone());
        self.probe
            .executions
            .lock()
            .unwrap()
            .push((self.id, account));
        if matches!(command, AccountCommand::ListProfiles) {
            self.probe.list_calls.fetch_add(1, Ordering::AcqRel);
            self.probe.gate.cross(Boundary::Execute, account);
            if self.probe.fail_list_once.swap(false, Ordering::AcqRel) {
                return Err(ServiceError::retryable("fixture_held_retry"));
            }
        }
        let mut state = AccountState {
            generation,
            stage: AccountStage::SignedIn,
            capability: true,
            profile_count: usize::from(account) * 11,
            ..Default::default()
        };
        let (credential, result) = match command {
            AccountCommand::Logout => {
                self.probe
                    .logout_sessions
                    .lock()
                    .unwrap()
                    .push(restored.session_id.clone());
                self.record = None;
                state.stage = AccountStage::SignedOut;
                (CredentialChange::Remove, AccountOperationResult::SignedOut)
            }
            AccountCommand::ListProfiles => (
                CredentialChange::Keep,
                AccountOperationResult::Profiles {
                    profiles: vec![AccountProfileRow {
                        koru_identity_id: owner(account).account.to_string(),
                        profile_id: format!("profile-{account}"),
                        name: Some(format!("Account {account}")),
                        locale: None,
                        auth_identities: vec![],
                    }],
                },
            ),
            AccountCommand::Retry => (
                CredentialChange::Replace(credential(
                    account,
                    self.probe.rotations.fetch_add(1, Ordering::Relaxed) + 1,
                )),
                AccountOperationResult::SignedIn,
            ),
            AccountCommand::Status => (CredentialChange::Keep, AccountOperationResult::SignedIn),
            AccountCommand::StartGoogle { .. } => {
                let account = self.probe.signin_account.load(Ordering::Acquire).max(1) as u8;
                state.profile_count = usize::from(account) * 11;
                let session = format!("session-{account}-new");
                self.session = Some(session.clone());
                (
                    CredentialChange::Replace(
                        SessionCredential::new(session, "new-signin-refresh".into()).unwrap(),
                    ),
                    AccountOperationResult::SignedIn,
                )
            }
            other => panic!("unexpected fixture command: {other:?}"),
        };
        Ok(ServiceOutcome {
            state,
            credential,
            result,
            follow_up: None,
        })
    }
}
struct FixedClock(Instant);
impl Clock for FixedClock {
    fn monotonic(&self) -> Instant {
        self.0
    }
    fn now_millis(&self) -> Option<i64> {
        Some(1_000)
    }
}
struct Harness {
    control: AccountControl,
    events: mpsc::Receiver<WorkerEvent>,
    results: HashMap<u64, (u64, AccountOperationResult)>,
    probe: Arc<Probe>,
    vault: Arc<Vault>,
    directory: Arc<MemoryAccountDirectory>,
}
impl Harness {
    fn new(boundary: Option<Boundary>) -> Self {
        Self::with_legacy(boundary, false)
    }
    fn with_legacy(boundary: Option<Boundary>, legacy_pending: bool) -> Self {
        let probe = Arc::new(Probe::default());
        Self::with_environment(
            boundary,
            legacy_pending,
            probe,
            Arc::new(FixedClock(Instant::now())),
        )
    }
    fn with_environment(
        boundary: Option<Boundary>,
        legacy_pending: bool,
        probe: Arc<Probe>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let vault = Arc::new(Vault {
            memory: SessionCredentialStore::default(),
            gate: probe.gate.clone(),
        });
        let directory = Arc::new(MemoryAccountDirectory::new());
        let entry = entry();
        if !legacy_pending {
            directory.retire_legacy(&entry.namespace()).unwrap();
        } else {
            vault
                .store(
                    &entry,
                    SessionCredential::new("session-2".into(), "legacy".into())
                        .unwrap()
                        .encode()
                        .unwrap(),
                )
                .unwrap();
        }
        for account in [1, 2] {
            let pending = directory
                .pending(&entry.namespace(), owner(account))
                .unwrap();
            vault
                .store(
                    &entry
                        .for_account(&owner(account).account.to_string())
                        .unwrap(),
                    credential(account, 0).encode().unwrap(),
                )
                .unwrap();
            directory
                .ready(
                    &entry.namespace(),
                    &pending,
                    pending.owner,
                    account == 1,
                    false,
                )
                .unwrap();
        }
        if let Some(boundary) = boundary {
            probe.gate.arm(boundary);
        }
        let (tx, events) = mpsc::channel();
        let control = AccountControl::start_with_directory(
            Service::new(probe.clone()),
            vault.clone(),
            entry,
            Some(directory.clone()),
            clock,
            false,
            move |event| {
                let _ = tx.send(event);
            },
        );
        Self {
            control,
            events,
            results: HashMap::new(),
            probe,
            vault,
            directory,
        }
    }
    fn receive(&mut self, deadline: Instant) {
        match self
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("coordinator notification before deadline")
        {
            WorkerEvent::Result {
                owner_scope,
                generation,
                result,
                ..
            } => {
                self.results.insert(generation, (owner_scope, result));
            }
            _ => {}
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
    fn command(&self, command: AccountCommand) -> u64 {
        self.control.enqueue(7, command).unwrap()
    }
    fn switch(&mut self, account: u8) -> u64 {
        let generation = self.command(AccountCommand::SwitchAccount {
            account_id: owner(account).account.to_string(),
        });
        let (scope, result) = self.result(generation);
        if let AccountOperationResult::Failed { code } = &result {
            panic!("switch account {account}, scope {scope}: {code}");
        }
        assert!(
            matches!(&result, AccountOperationResult::AccountSelected { scope_id, account_id: Some(id) } if *scope_id == scope && *id == owner(account).account.to_string()),
            "{result:?}"
        );
        assert_eq!(
            self.control.state().active_account_id,
            Some(owner(account).account.to_string())
        );
        scope
    }
    fn ready(&mut self) {
        self.control.set_foreground(true);
        let deadline = Instant::now() + WAIT;
        while self
            .control
            .state()
            .held_accounts
            .iter()
            .filter(|h| h.stage == AccountStage::SignedIn)
            .count()
            != 2
        {
            self.receive(deadline);
        }
        // A selected service response is a FIFO fence after preceding Finished messages.
        let generation = self.command(AccountCommand::Status);
        self.result(generation);
    }
    fn bytes(&self, account: u8) -> Option<SecretString> {
        self.vault
            .load(
                &entry()
                    .for_account(&owner(account).account.to_string())
                    .unwrap(),
            )
            .unwrap()
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.probe.gate.release();
    }
}

#[test]
fn switch_ack_and_b_projection_do_not_wait_for_a_service_or_vault() {
    for boundary in [Boundary::Execute, Boundary::Vault] {
        let mut h = Harness::new(None);
        h.ready();
        let a_scope = h.control.state().active_scope;
        let b_bytes = h.bytes(2).unwrap();
        h.probe.gate.arm(boundary);
        let original = h.command(if boundary == Boundary::Execute {
            AccountCommand::ListProfiles
        } else {
            AccountCommand::Retry
        });
        h.probe.gate.entered();
        let b_scope = h.switch(2);
        assert_ne!(a_scope, b_scope);
        assert_eq!(h.control.state().profile_count, 22);
        assert!(
            h.control
                .state()
                .held_accounts
                .iter()
                .any(|a| a.scope_id == a_scope && a.account_id == owner(1).account.to_string())
        );
        h.probe.gate.release();
        let (scope, result) = h.result(original);
        assert_eq!(scope, a_scope, "late result retains its execution owner");
        if boundary == Boundary::Execute {
            assert!(
                matches!(result, AccountOperationResult::Profiles { profiles } if profiles[0].koru_identity_id == owner(1).account.to_string())
            );
        } else {
            assert!(matches!(result, AccountOperationResult::SignedIn));
        }
        assert_eq!(h.control.state().active_scope, b_scope);
        assert_eq!(h.control.state().profile_count, 22);
        assert_eq!(h.bytes(2).unwrap().expose(), b_bytes.expose());
        assert_eq!(h.switch(1), a_scope);
        assert_eq!(h.control.state().profile_count, 11);
    }
}

#[test]
fn switching_during_startup_inspection_does_not_restore_b_into_a_scope() {
    let mut h = Harness::new(Some(Boundary::Inspect));
    h.control.set_foreground(true);
    h.probe.gate.entered();
    let a_scope = h.control.state().active_scope;
    h.switch(2);
    h.probe.gate.release();
    h.ready();
    assert!(
        h.probe
            .executions
            .lock()
            .unwrap()
            .iter()
            .any(|&(id, account)| id == 0 && account == 1)
    );
    assert!(
        !h.probe
            .executions
            .lock()
            .unwrap()
            .iter()
            .any(|&(id, account)| id == 0 && account == 2)
    );
    assert_eq!(h.switch(1), a_scope);
    assert_eq!(h.control.state().profile_count, 11);
}

#[test]
fn selecting_b_before_initial_restore_keeps_captured_a_owner() {
    let mut h = Harness::new(None);
    let deadline = Instant::now() + WAIT;
    while h.control.state().held_accounts.len() != 2 {
        h.receive(deadline);
    }
    let a_scope = h.control.state().active_scope;
    h.switch(2);
    h.ready();
    let executions = h.probe.executions.lock().unwrap();
    assert!(
        executions
            .iter()
            .any(|&(service, account)| service == 0 && account == 1)
    );
    assert!(
        !executions
            .iter()
            .any(|&(service, account)| service == 0 && account == 2)
    );
    drop(executions);
    assert_eq!(h.switch(1), a_scope);
}

#[test]
fn selection_stays_pinned_through_legacy_inspection_and_directory_reread() {
    let mut h = Harness::with_legacy(Some(Boundary::InspectLegacy), true);
    h.control.set_foreground(true);
    h.probe.gate.entered();
    let a_scope = h.control.state().active_scope;
    h.switch(2);
    h.probe.gate.release();
    h.ready();
    let executions = h.probe.executions.lock().unwrap();
    assert!(
        executions
            .iter()
            .any(|&(service, account)| service == 0 && account == 1)
    );
    assert!(
        !executions
            .iter()
            .any(|&(service, account)| service == 0 && account == 2)
    );
    drop(executions);
    assert_eq!(h.switch(1), a_scope);
    assert!(
        h.directory
            .snapshot(&entry().namespace())
            .unwrap()
            .legacy_retired
    );
    assert!(h.vault.load(&entry()).unwrap().is_none());
}

#[test]
fn repeated_add_resumes_the_admitted_anonymous_scope_without_a_second_start() {
    let mut h = Harness::new(None);
    let first = h
        .control
        .enqueue_owned(
            7,
            None,
            AccountCommand::AddAccount {
                locale: "en".into(),
            },
        )
        .unwrap();
    let second = h
        .control
        .enqueue_owned(
            7,
            None,
            AccountCommand::AddAccount {
                locale: "en".into(),
            },
        )
        .unwrap();
    assert_eq!(first.owner_scope, second.owner_scope);
    let (scope, result) = h.result(second.generation);
    assert_eq!(scope, first.owner_scope);
    assert!(
        matches!(result, AccountOperationResult::AccountSelected { scope_id, account_id: None } if scope_id == scope)
    );
    assert!(h.control.requester_generation_active(7, first.generation));
    assert!(!h.control.requester_generation_active(7, second.generation));
    assert_eq!(h.control.state().queue_depth, 1);
    assert!(
        h.probe.executions.lock().unwrap().is_empty(),
        "backgrounded resume must not execute provider work"
    );
}

#[test]
fn add_during_held_initial_legacy_restore_keeps_a_separate_user_intent() {
    let mut h = Harness::with_legacy(Some(Boundary::InspectLegacy), true);
    h.probe.signin_account.store(3, Ordering::Release);
    h.control.set_foreground(true);
    h.probe.gate.entered();
    let initial = h.control.state().active_scope;
    let first = h
        .control
        .enqueue_owned(
            7,
            None,
            AccountCommand::AddAccount {
                locale: "en".into(),
            },
        )
        .unwrap();
    let again = h
        .control
        .enqueue_owned(
            7,
            None,
            AccountCommand::AddAccount {
                locale: "en".into(),
            },
        )
        .unwrap();
    assert_eq!(first.owner_scope, again.owner_scope);
    assert_ne!(first.owner_scope, initial);
    let (_, resumed) = h.result(again.generation);
    assert!(matches!(
        resumed,
        AccountOperationResult::AccountSelected {
            account_id: None,
            ..
        }
    ));
    assert_eq!(h.control.state().active_scope, first.owner_scope);
    assert!(h.control.state().active_account_id.is_none());
    h.probe.gate.release();
    let (scope, result) = h.result(first.generation);
    assert_eq!(scope, first.owner_scope);
    assert!(matches!(result, AccountOperationResult::SignedIn));
    assert_eq!(
        h.control.state().active_account_id,
        Some(owner(3).account.to_string())
    );
    assert!(
        h.control
            .state()
            .held_accounts
            .iter()
            .any(|row| row.account_id == owner(2).account.to_string() && row.scope_id != scope)
    );
    assert_eq!(h.switch(1), initial);
    assert!(h.vault.load(&entry()).unwrap().is_none());
    assert!(h.bytes(1).is_some() && h.bytes(2).is_some() && h.bytes(3).is_some());
}

#[test]
fn newly_authenticated_same_account_retires_old_retry_without_erasing_new_slot() {
    let mut h = Harness::new(None);
    h.ready();
    let old_scope = h.control.state().active_scope;
    let b_bytes = h.bytes(2).unwrap();
    h.probe.fail_list_once.store(true, Ordering::Release);
    let old = h.command(AccountCommand::ListProfiles);
    let (_, answer) = h.result(old);
    assert!(
        matches!(&answer, AccountOperationResult::Failed { code } if code == "fixture_held_retry"),
        "{answer:?}"
    );
    let deadline = Instant::now() + WAIT;
    while h.control.state().retry_at_millis.is_none() {
        h.receive(deadline);
    }
    let add = h
        .control
        .enqueue_owned(
            7,
            None,
            AccountCommand::AddAccount {
                locale: "en".into(),
            },
        )
        .unwrap();
    let (scope, result) = h.result(add.generation);
    assert_eq!(scope, add.owner_scope);
    assert_ne!(scope, old_scope);
    assert!(matches!(result, AccountOperationResult::SignedIn));
    let deadline = Instant::now() + WAIT;
    while h.control.requester_generation_active(7, old) {
        assert!(
            Instant::now() < deadline,
            "obsolete retry must settle after owned cleanup"
        );
        std::thread::yield_now();
    }
    // A command on the selected fresh scope fences the preceding old cleanup job.
    let fence = h.command(AccountCommand::Status);
    h.result(fence);
    let stored = SessionCredential::decode(&h.bytes(1).unwrap()).unwrap();
    assert_eq!(stored.session_id, "session-1-new");
    assert_eq!(stored.refresh_token, "new-signin-refresh");
    assert_eq!(h.bytes(2).unwrap().expose(), b_bytes.expose());
    assert_eq!(h.control.state().active_scope, scope);
    h.switch(2);
    assert_eq!(
        h.switch(1),
        scope,
        "retired scope must not shadow the replacement account route"
    );
    assert_eq!(
        h.control
            .state()
            .held_accounts
            .iter()
            .filter(|a| a.account_id == owner(1).account.to_string())
            .count(),
        1
    );
}

#[test]
fn repair_signin_as_another_account_is_cleaned_up_before_touching_either_slot() {
    let mut h = Harness::new(None);
    h.ready();
    let original_scope = h.control.state().active_scope;
    let a_bytes = h.bytes(1).unwrap();
    let b_bytes = h.bytes(2).unwrap();
    h.probe.signin_account.store(2, Ordering::Release);
    let generation = h.command(AccountCommand::StartGoogle {
        locale: "en".into(),
    });
    let (scope, result) = h.result(generation);
    assert_eq!(scope, original_scope);
    assert!(
        matches!(result, AccountOperationResult::Failed { code } if code == "account_repair_identity_mismatch")
    );
    assert_eq!(h.bytes(1).unwrap().expose(), a_bytes.expose());
    let restored_a_scope = h.switch(1);
    assert_ne!(restored_a_scope, original_scope);
    assert_ne!(h.switch(2), original_scope);
    assert_eq!(h.bytes(2).unwrap().expose(), b_bytes.expose());
}

#[test]
fn cancel_or_logout_during_repair_inspection_keeps_the_captured_cleanup_owner() {
    for explicit_logout in [false, true] {
        let mut h = Harness::new(None);
        h.ready();
        let a_bytes = h.bytes(1).unwrap();
        let b_bytes = h.bytes(2).unwrap();
        let old_b = h.switch(2);
        h.probe.signin_account.store(1, Ordering::Release);
        h.probe.gate.arm(Boundary::Inspect);
        let signin = h.command(AccountCommand::StartGoogle {
            locale: "en".into(),
        });
        h.probe.gate.entered();
        let cleanup = h.command(if explicit_logout {
            AccountCommand::Logout
        } else {
            AccountCommand::Cancel { generation: signin }
        });
        let a_scope = h.switch(1);
        assert_eq!(h.bytes(1).unwrap().expose(), a_bytes.expose());
        h.probe.gate.release();
        let (scope, result) = h.result(cleanup);
        if let AccountOperationResult::Failed { code } = &result {
            panic!("cleanup logout={explicit_logout}, scope {scope}: {code}");
        }
        assert_eq!(scope, old_b);
        assert!(
            matches!(
                result,
                AccountOperationResult::SignedOut | AccountOperationResult::Cancelled
            ),
            "{explicit_logout}: {result:?}"
        );
        assert_eq!(h.control.state().active_scope, a_scope);
        assert_eq!(h.bytes(1).unwrap().expose(), a_bytes.expose());
        assert!(
            h.probe
                .logout_sessions
                .lock()
                .unwrap()
                .iter()
                .any(|session| session == "session-1-new"),
            "cleanup revokes the newly issued attempt, not A's older surviving session"
        );
        assert!(
            !h.probe
                .logout_sessions
                .lock()
                .unwrap()
                .iter()
                .any(|session| session == "session-1")
        );
        if explicit_logout {
            assert!(h.bytes(2).is_none());
            assert!(
                !h.control
                    .state()
                    .held_accounts
                    .iter()
                    .any(|held| held.account_id == owner(2).account.to_string())
            );
        } else {
            assert_eq!(h.bytes(2).unwrap().expose(), b_bytes.expose());
            assert_ne!(
                h.switch(2),
                old_b,
                "surviving B remains discoverable after the cancelled repair scope retires"
            );
        }
    }
}

#[test]
fn attaching_responder_after_restore_publishes_all_acknowledged_held_accounts() {
    let mut h = Harness::new(None);
    h.ready();
    let gate = Arc::new(ResponderSessionGate::new());
    h.control.set_responder_session_gate(gate.clone());
    let deadline = Instant::now() + WAIT;
    while gate.ticket(owner(1).account).is_err() || gate.ticket(owner(2).account).is_err() {
        assert!(
            Instant::now() < deadline,
            "late attachment must wake idle coordinator"
        );
        std::thread::yield_now();
    }
    assert!(gate.ticket(owner(1).account).is_ok());
    assert!(gate.ticket(owner(2).account).is_ok());
}

#[test]
fn late_a_vault_ack_after_logout_admission_cannot_reenable_a_or_erase_b() {
    let mut h = Harness::new(None);
    h.ready();
    let gate = Arc::new(ResponderSessionGate::new());
    h.control.set_responder_session_gate(gate.clone());
    let fence = h.command(AccountCommand::Status);
    h.result(fence);
    let b_ticket = gate.ticket(owner(2).account).unwrap();
    let b_bytes = h.bytes(2).unwrap();
    h.probe.gate.arm(Boundary::Vault);
    h.command(AccountCommand::Retry);
    h.probe.gate.entered();
    let logout = h.command(AccountCommand::Logout);
    h.switch(2); // Coordinator FIFO fence: removal admitted while A's vault is held.
    assert!(gate.ticket(owner(1).account).is_err());
    assert!(gate.ensure(b_ticket).is_ok());
    h.probe.gate.release();
    let (_, result) = h.result(logout);
    assert!(matches!(result, AccountOperationResult::SignedOut));
    let fence = h.command(AccountCommand::Status);
    h.result(fence);
    assert!(gate.ticket(owner(1).account).is_err());
    assert!(gate.ensure(b_ticket).is_ok());
    assert!(h.bytes(1).is_none());
    assert_eq!(h.bytes(2).unwrap().expose(), b_bytes.expose());
    assert_eq!(
        h.control.state().active_account_id,
        Some(owner(2).account.to_string())
    );
}

#[test]
fn selecting_a_while_its_replacement_is_pending_does_not_publish_unacknowledged_authority() {
    let mut h = Harness::new(None);
    h.ready();
    let a_scope = h.control.state().active_scope;
    let gate = Arc::new(ResponderSessionGate::new());
    h.control.set_responder_session_gate(gate.clone());
    let fence = h.command(AccountCommand::Status);
    h.result(fence);
    let ticket = gate.ticket(owner(1).account).unwrap();
    let before = h.probe.acknowledged_records.lock().unwrap().clone();
    h.probe.gate.arm(Boundary::Vault);
    let refresh = h.command(AccountCommand::Retry);
    h.probe.gate.entered();
    let pending = h
        .directory
        .snapshot(&entry().namespace())
        .unwrap()
        .records
        .into_iter()
        .find(|row| row.owner.account == owner(1).account)
        .unwrap();
    assert_eq!(pending.state, crate::identity::DirectoryState::Pending);
    h.switch(2);
    assert_eq!(h.switch(1), a_scope);
    assert!(
        gate.ensure(ticket).is_ok(),
        "the previously acknowledged session remains eligible"
    );
    assert_eq!(
        *h.probe.acknowledged_records.lock().unwrap(),
        before,
        "view selection must not acknowledge the new credential revision"
    );
    h.probe.gate.release();
    assert!(matches!(
        h.result(refresh).1,
        AccountOperationResult::SignedIn
    ));
    assert!(
        h.probe
            .acknowledged_records
            .lock()
            .unwrap()
            .iter()
            .any(|row| row.owner.account == owner(1).account && row.version == pending.version)
    );
}

#[test]
fn stale_old_owner_clear_refuses_before_touching_recreated_ready_slot() {
    let h = Harness::new(None);
    let mut old = CredentialPersistence::new(h.vault.clone(), entry(), Some(h.directory.clone()))
        .for_selection(Some(owner(1).account));
    let mut service = Service::new(h.probe.clone());
    old.restore(&mut service).unwrap();
    h.directory
        .remove(&entry().namespace(), &old.record().unwrap())
        .unwrap();
    let mut replacement =
        CredentialPersistence::new(h.vault.clone(), entry(), Some(h.directory.clone()))
            .for_selection(Some(owner(1).account));
    let mut new_session = credential(1, 99);
    new_session.session_id = "session-1-replacement".into();
    replacement
        .replace(
            &new_session,
            &mut PendingCredentialWrite::default(),
            &mut service,
        )
        .unwrap();
    let before = h.directory.snapshot(&entry().namespace()).unwrap();
    let bytes = h.bytes(1).unwrap();
    assert_eq!(
        old.clear(None, true).unwrap_err().code,
        "account_directory_conflict"
    );
    assert_eq!(h.bytes(1).unwrap().expose(), bytes.expose());
    let after = h.directory.snapshot(&entry().namespace()).unwrap();
    assert_eq!(after.records, before.records);
    assert_eq!(after.active, before.active);
    assert_eq!(after.legacy_retired, before.legacy_retired);
}

/// The maintainer's recurring "sign in again on every launch": the stored
/// slot's inspection is refused because the rotation the server answered never
/// reached the vault, so the stored refresh token is no longer the newest one.
/// The refusal must not end the start. The credential goes to the scheduled
/// retry, whose refresh replays the rotation under the key
/// `refresh_retry_key` derives from these same old bytes, and the person ends
/// signed in without ever being shown a sign-in prompt.
#[test]
fn a_refused_inspection_is_replayed_by_the_refresh_instead_of_asking_for_a_sign_in() {
    let probe = Arc::new(Probe::default());
    *probe.rejected_refresh.lock().unwrap() = Some("refresh-1-0".into());
    let mut h = Harness::with_environment(None, false, probe, Arc::new(FixedClock(Instant::now())));
    let stored = h.bytes(1).unwrap();
    h.control.set_foreground(true);
    let deadline = Instant::now() + WAIT;
    let mut stages = vec![h.control.state().stage];
    while h.control.state().stage != AccountStage::SignedIn {
        h.receive(deadline);
        stages.push(h.control.state().stage);
    }
    assert!(
        !stages
            .iter()
            .any(|stage| matches!(stage, AccountStage::Failed | AccountStage::SignedOut)),
        "a refused inspection must not flash a sign-in surface: {stages:?}"
    );
    let state = h.control.state();
    assert_eq!(state.stage, AccountStage::SignedIn);
    assert_eq!(
        state.failure_code, None,
        "the replayed rotation repaired the slot, so no code survives it"
    );
    assert_ne!(
        h.bytes(1).unwrap().expose(),
        stored.expose(),
        "the refreshed credential replaced the superseded bytes in the vault"
    );
}

/// The same refusal, but the session really is gone: the refresh route refuses
/// it too. That is the only proof this code accepts, and it must still land
/// the person on the re-authenticate surface — one round trip later — with the
/// vault erased so the next launch does not present the dead slot again.
#[test]
fn a_session_the_refresh_route_refuses_ends_signed_out_with_the_vault_cleared() {
    let probe = Arc::new(Probe::default());
    *probe.rejected_refresh.lock().unwrap() = Some("refresh-1-0".into());
    *probe.revoked_refresh.lock().unwrap() = Some("refresh-1-0".into());
    let mut h = Harness::with_environment(None, false, probe, Arc::new(FixedClock(Instant::now())));
    h.control.set_foreground(true);
    let deadline = Instant::now() + WAIT;
    while h.control.state().stage != AccountStage::SignedOut {
        h.receive(deadline);
    }
    assert_eq!(
        h.control.state().failure_code.as_deref(),
        Some("session_revoked"),
        "the re-authenticate surface is reached through the refresh route's refusal"
    );
    assert!(
        h.bytes(1).is_none(),
        "a revoked session's bytes must not survive to be refused again"
    );
}

#[test]
fn signing_in_again_after_a_rejected_slot_clears_the_repair_code() {
    let probe = Arc::new(Probe::default());
    *probe.rejected_refresh.lock().unwrap() = Some("refresh-1-0".into());
    *probe.revoked_refresh.lock().unwrap() = Some("refresh-1-0".into());
    let mut h = Harness::with_environment(None, false, probe, Arc::new(FixedClock(Instant::now())));
    h.control.set_foreground(true);
    let deadline = Instant::now() + WAIT;
    // The inspection refusal left a repair code behind; the refresh route then
    // proved the session dead. The code outlives the sign-out, so the sign-in
    // that repairs the slot is what has to clear it.
    while h.control.state().stage != AccountStage::SignedOut {
        h.receive(deadline);
    }

    h.probe.signin_account.store(1, Ordering::Release);
    let generation = h.command(AccountCommand::StartGoogle {
        locale: "en".into(),
    });
    let (_, result) = h.result(generation);
    assert!(
        matches!(result, AccountOperationResult::SignedIn),
        "{result:?}"
    );
    let state = h.control.state();
    assert_eq!(state.stage, AccountStage::SignedIn);
    assert_eq!(
        state.failure_code, None,
        "a stale repair code must not outlive the sign-in that repaired it"
    );
}
