// SPDX-License-Identifier: MPL-2.0

//! Shared adapter fixture: real schema-5 directory/target transactions and the
//! real scheduler, with injected hosted sessions and ceremony completions.
//! No Store, filesystem, provider, or network is opened. Signed ceremony
//! verification is covered separately by the production transport fixtures.
use super::*;
use crate::account::directory::AccountDirectory;
use crate::account::{
    AccountService, Cancel, CredentialChange, ServiceError, ServiceOutcome, ServiceProgress,
};
use crate::identity::{
    AccountDirectorySnapshot, AccountOwner, DirectoryRecord, SqliteTrustRepository,
};
use koru_agent_wire::{AccountCommand, AccountOperationResult, AccountStage, AccountState};
use koru_oauth_credentials::{
    CredentialContext, CredentialEntry, CredentialStore, SessionCredential, SessionCredentialStore,
};
use std::sync::Condvar;

pub fn owner(account: u8) -> AccountOwner {
    AccountOwner {
        account: format!("10000000-0000-4000-8000-{account:012}")
            .parse()
            .unwrap(),
        auth_identity: format!("20000000-0000-4000-8000-{account:012}")
            .parse()
            .unwrap(),
    }
}
pub fn profile(id: u8) -> ProfileId {
    ProfileId::parse(&format!("30000000-0000-4000-8000-{id:012}")).unwrap()
}
pub fn credential(account: u8) -> SessionCredential {
    SessionCredential::new(
        format!("40000000-0000-4000-8000-{account:012}"),
        format!("injected-refresh-{account}"),
    )
    .unwrap()
}
fn account_number(value: &SessionCredential) -> u8 {
    value
        .session_id
        .rsplit('-')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}
fn grants(account: u8) -> BTreeSet<ProfileId> {
    if account == 1 {
        [profile(1), profile(3), profile(4)].into()
    } else {
        [profile(2), profile(5)].into()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    Load,
    Enroll,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Load {
        account: u8,
        version: i64,
    },
    Discover {
        account: u8,
        profile: ProfileId,
    },
    Commit {
        account: u8,
        profile: ProfileId,
        revision: u64,
    },
    Provision {
        account: u8,
        profile: ProfileId,
    },
    Discard(ProfileId),
}
#[derive(Default)]
struct GateState {
    armed: Option<(Boundary, u8)>,
    entered: bool,
    released: bool,
}
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}
impl Gate {
    fn cross(&self, boundary: Boundary, account: u8) {
        let mut state = self.state.lock().unwrap();
        if state.armed != Some((boundary, account)) {
            return;
        }
        state.armed = None;
        state.entered = true;
        self.changed.notify_all();
        while !state.released {
            state = self.changed.wait(state).unwrap();
        }
    }
    fn release(&self) {
        self.state.lock().unwrap().released = true;
        self.changed.notify_all();
    }
}

/// Directory and target operations share this exact in-memory connection.
pub struct FixtureDirectory(Mutex<SqliteTrustRepository>);
impl FixtureDirectory {
    fn with<T>(
        &self,
        op: impl FnOnce(&mut SqliteTrustRepository) -> Result<T, koru_device::DeviceError>,
    ) -> Result<T, ServiceError> {
        op(&mut self.0.lock().unwrap())
            .map_err(|_| ServiceError::retryable("account_directory_unavailable"))
    }
}
impl AccountDirectory for FixtureDirectory {
    fn activate(
        &self,
        namespace: &str,
        account: koru_account_core::KoruIdentityId,
    ) -> Result<(), ServiceError> {
        self.with(|r| r.activate_account(namespace, account))
    }
    fn snapshot(&self, namespace: &str) -> Result<AccountDirectorySnapshot, ServiceError> {
        self.with(|r| r.account_directory(namespace))
    }
    fn pending(
        &self,
        namespace: &str,
        owner: AccountOwner,
    ) -> Result<DirectoryRecord, ServiceError> {
        self.with(|r| r.begin_account_write(namespace, owner))
    }
    fn ready(
        &self,
        namespace: &str,
        record: &DirectoryRecord,
        actual: AccountOwner,
        activate: bool,
        retire: bool,
    ) -> Result<(), ServiceError> {
        self.with(|r| r.finish_account_write(namespace, record, actual, activate, retire))
    }
    fn remove(&self, namespace: &str, record: &DirectoryRecord) -> Result<(), ServiceError> {
        self.with(|r| r.remove_account(namespace, record))
    }
    fn retire_legacy(&self, namespace: &str) -> Result<(), ServiceError> {
        self.with(|r| r.retire_legacy_credential(namespace))
    }
    fn deactivate(&self, namespace: &str, owner: AccountOwner) -> Result<(), ServiceError> {
        self.with(|r| r.deactivate_account(namespace, owner.account))
    }
}
struct Shared {
    directory: Arc<FixtureDirectory>,
    gate: Gate,
    trace: Mutex<Vec<Effect>>,
    completions: Mutex<BTreeMap<ProfileId, u64>>,
    revisions: Mutex<Vec<u64>>,
}
impl Shared {
    fn account(&self, authority: &EnrollmentAuthorization) -> u8 {
        if authority.owner.account == owner(1).account {
            1
        } else {
            assert_eq!(authority.owner.account, owner(2).account);
            2
        }
    }
}
#[derive(Clone)]
pub struct FixturePort(Arc<Shared>);
impl Port for FixturePort {
    type Scope = (String, koru_account_core::KoruIdentityId);
    type Credential = EnrollmentAuthorization;
    type Candidate = u8;
    type Attempt = ProfileId;
    type Completion = ProfileId;
    fn fork_effects(&self) -> Result<Self, Failure> {
        Ok(self.clone())
    }
    fn scope(value: &EnrollmentAuthorization) -> Self::Scope {
        (value.namespace.clone(), value.owner.account)
    }
    fn snapshot_account_id(value: &EnrollmentAuthorization) -> Option<String> {
        Some(value.owner.account.to_string())
    }
    fn same_authority(a: &EnrollmentAuthorization, b: &EnrollmentAuthorization) -> bool {
        a.namespace == b.namespace
            && a.owner == b.owner
            && a.record_version == b.record_version
            && a.session_id == b.session_id
    }
    fn authority_successor(a: &EnrollmentAuthorization, b: &EnrollmentAuthorization) -> bool {
        a.namespace == b.namespace
            && a.owner.account == b.owner.account
            && b.record_version > a.record_version
    }
    fn refresh_continuity(a: &EnrollmentAuthorization, b: &EnrollmentAuthorization) -> bool {
        a.namespace == b.namespace
            && a.owner == b.owner
            && a.session_id == b.session_id
            && b.record_version > a.record_version
    }
    fn restored_targets(
        &mut self,
        authority: &EnrollmentAuthorization,
    ) -> Result<BTreeSet<ProfileId>, Failure> {
        let account = self.0.account(authority);
        self.0.trace.lock().unwrap().push(Effect::Load {
            account,
            version: authority.record_version,
        });
        self.0.gate.cross(Boundary::Load, account);
        let mut repository = self.0.directory.0.lock().unwrap();
        let targets = repository
            .account_targets(&authority.namespace, authority.owner.account)
            .map_err(|_| Failure::Persistence)?;
        repository
            .validate_account_targets(
                &authority.namespace,
                authority.owner,
                authority.record_version,
                &targets,
            )
            .map_err(|_| Failure::Persistence)?;
        Ok(targets)
    }
    fn refresh_authority(
        &mut self,
        a: &EnrollmentAuthorization,
        b: &EnrollmentAuthorization,
        expected: &BTreeSet<ProfileId>,
    ) -> Result<BTreeSet<ProfileId>, Failure> {
        if !Self::refresh_continuity(a, b) {
            return Err(Failure::SessionExpired);
        }
        self.0
            .directory
            .0
            .lock()
            .unwrap()
            .validate_account_targets(&b.namespace, b.owner, b.record_version, expected)
            .map_err(|_| Failure::Persistence)?;
        Ok(expected
            .difference(&grants(self.0.account(b)))
            .copied()
            .collect())
    }
    fn replace_targets(
        &mut self,
        authority: &EnrollmentAuthorization,
        expected: &BTreeSet<ProfileId>,
        next: &BTreeSet<ProfileId>,
    ) -> Result<(), Failure> {
        if !next.is_subset(&grants(self.0.account(authority))) {
            return Err(Failure::AccessRevoked);
        }
        self.0
            .directory
            .0
            .lock()
            .unwrap()
            .replace_account_targets(
                &authority.namespace,
                authority.owner,
                authority.record_version,
                expected,
                next,
            )
            .map_err(|_| Failure::Persistence)
    }
    fn authorize_profile(
        &mut self,
        profile: ProfileId,
        authority: &EnrollmentAuthorization,
    ) -> Result<bool, Failure> {
        Ok(grants(self.0.account(authority)).contains(&profile))
    }
    fn reconcile(&mut self, profile: ProfileId) -> Result<Option<Completion<ProfileId>>, Failure> {
        Ok(self
            .0
            .completions
            .lock()
            .unwrap()
            .get(&profile)
            .map(|revision| Completion {
                value: profile,
                durable_revision: *revision,
            }))
    }
    fn discover(
        &mut self,
        profile: ProfileId,
        authority: &EnrollmentAuthorization,
        _: &AtomicBool,
    ) -> Result<Discovery<u8, ProfileId>, Failure> {
        let account = self.0.account(authority);
        self.0
            .trace
            .lock()
            .unwrap()
            .push(Effect::Discover { account, profile });
        Ok(Discovery {
            candidates: vec![account],
            only_older_peers: false,
            attempt: profile,
        })
    }
    fn enroll(
        &mut self,
        profile: ProfileId,
        candidate: &u8,
        attempt: &ProfileId,
        _: &AtomicBool,
    ) -> Result<Completion<ProfileId>, Failure> {
        assert_eq!(profile, *attempt);
        self.0.gate.cross(Boundary::Enroll, *candidate);
        // The injected effect deliberately finishes after cancellation. This
        // exercises publication of a committed revision from retired work.
        let mut completions = self.0.completions.lock().unwrap();
        let next = completions.values().copied().max().unwrap_or(0) + 1;
        let revision = *completions.entry(profile).or_insert(next);
        self.0.trace.lock().unwrap().push(Effect::Commit {
            account: *candidate,
            profile,
            revision,
        });
        Ok(Completion {
            value: profile,
            durable_revision: revision,
        })
    }
    fn provision(
        &mut self,
        profile: ProfileId,
        completion: &ProfileId,
        authority: &EnrollmentAuthorization,
    ) -> Result<(), Failure> {
        assert_eq!(profile, *completion);
        self.0.trace.lock().unwrap().push(Effect::Provision {
            account: self.0.account(authority),
            profile,
        });
        Ok(())
    }
    fn discard(&mut self, profile: ProfileId) {
        self.0.trace.lock().unwrap().push(Effect::Discard(profile));
    }
}

pub struct FixtureService {
    enrollment: Arc<Control<FixturePort>>,
    namespace: String,
    inspected: Option<(u8, String)>,
    authority: Option<EnrollmentAuthorization>,
}
impl AccountService for FixtureService {
    fn fork(&self) -> Option<Box<dyn AccountService>> {
        Some(Box::new(Self {
            enrollment: self.enrollment.clone(),
            namespace: self.namespace.clone(),
            inspected: None,
            authority: None,
        }))
    }
    fn inspect_credential(
        &mut self,
        credential: &SessionCredential,
    ) -> Result<AccountOwner, ServiceError> {
        self.inspected = Some((account_number(credential), credential.session_id.clone()));
        Ok(owner(account_number(credential)))
    }
    fn credential_owner_acknowledged(&mut self, record: Option<DirectoryRecord>) {
        let record = record.expect("directory acknowledgement is required");
        let inspected = self
            .inspected
            .as_ref()
            .expect("current-secret inspection precedes acknowledgement");
        assert_eq!(record.owner, owner(inspected.0));
        self.authority = Some(EnrollmentAuthorization {
            access: koru_account_wire::AccessToken("injected-access".into()),
            owner: record.owner,
            namespace: self.namespace.clone(),
            record_version: record.version,
            session_id: inspected.1.clone(),
        });
    }
    fn enrollment_authority(&self) -> Option<EnrollmentAuthorization> {
        self.authority.clone()
    }
    fn execute(
        &mut self,
        generation: u64,
        command: AccountCommand,
        restored: Option<&SessionCredential>,
        _: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        let credential =
            restored.ok_or_else(|| ServiceError::terminal("fixture_session_missing"))?;
        let account = account_number(credential);
        let mut state = AccountState {
            generation,
            capability: true,
            stage: AccountStage::SignedIn,
            profile_count: grants(account).len(),
            ..Default::default()
        };
        let mut change = CredentialChange::Keep;
        let result = match command {
            AccountCommand::Retry => {
                self.inspected = Some((account, credential.session_id.clone()));
                // Deliberately replay the fixture's bytes through real vault
                // durability without making production credentials Clone.
                change = CredentialChange::Replace(
                    SessionCredential::decode(&credential.encode().unwrap()).unwrap(),
                );
                AccountOperationResult::SignedIn
            }
            AccountCommand::SelectProfiles { profile_ids } => {
                let profiles = profile_ids
                    .iter()
                    .map(|value| ProfileId::parse(value).unwrap())
                    .collect();
                let snapshot = self
                    .enrollment
                    .select_profiles_durable(
                        profiles,
                        self.authority
                            .clone()
                            .expect("selection follows durable acknowledgement"),
                    )
                    .map_err(ServiceError::terminal)?;
                AccountOperationResult::ProfilesSelected {
                    profiles: snapshot.profiles.iter().map(ProfileReport::wire).collect(),
                }
            }
            AccountCommand::RetryEnrollment { profile_id } => {
                self.enrollment.retry_for(
                    self.authority.clone().unwrap(),
                    profile_id.map(|id| ProfileId::parse(&id).unwrap()),
                );
                AccountOperationResult::SignedIn
            }
            AccountCommand::CancelEnrollment => {
                self.enrollment
                    .cancel_durable_for(self.authority.clone().unwrap())
                    .map_err(ServiceError::terminal)?;
                AccountOperationResult::SignedIn
            }
            AccountCommand::Logout => {
                change = CredentialChange::Remove;
                state.stage = AccountStage::SignedOut;
                self.authority = None;
                AccountOperationResult::SignedOut
            }
            AccountCommand::Status
            | AccountCommand::ListProfiles
            | AccountCommand::ListProfileChoices => AccountOperationResult::SignedIn,
            _ => return Err(ServiceError::terminal("fixture_command_unsupported")),
        };
        Ok(ServiceOutcome {
            state,
            credential: change,
            result,
            follow_up: None,
        })
    }
}

pub struct Fixture {
    pub entry: CredentialEntry,
    pub directory: Arc<FixtureDirectory>,
    pub vault: Arc<SessionCredentialStore>,
    pub enrollment: Arc<Control<FixturePort>>,
    shared: Arc<Shared>,
}
impl Fixture {
    pub fn new() -> Self {
        let entry = CredentialContext::new(
            "test",
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/enrollment-adapter-fixture"),
            koru_device::DeviceId::parse("50000000-0000-4000-8000-000000000001").unwrap(),
        )
        .unwrap()
        .entry("https://account.invalid")
        .unwrap();
        let directory = Arc::new(FixtureDirectory(Mutex::new(
            SqliteTrustRepository::in_memory().unwrap(),
        )));
        let vault = Arc::new(SessionCredentialStore::default());
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
            directory
                .0
                .lock()
                .unwrap()
                .replace_account_targets(
                    &entry.namespace(),
                    record.owner,
                    record.version,
                    &BTreeSet::new(),
                    &BTreeSet::from([profile(account)]),
                )
                .unwrap();
            vault
                .store(
                    &entry
                        .for_account(&owner(account).account.to_string())
                        .unwrap(),
                    // A slot as the code writes one: the token and the random
                    // key a refresh of it must present as its idempotency
                    // key, in the one value. Seeding the token alone is a slot
                    // from a build that kept no key, which restore then has to
                    // top up — a write this fixture would see as the vault
                    // being rewritten behind it.
                    crate::account::directory::encode_slot(&credential(account), &[account; 32])
                        .unwrap(),
                )
                .unwrap();
        }
        let shared = Arc::new(Shared {
            directory: directory.clone(),
            gate: Gate::default(),
            trace: Mutex::default(),
            completions: Mutex::default(),
            revisions: Mutex::default(),
        });
        let events = shared.clone();
        let enrollment = Arc::new(Control::start_with_clock_and_foreground(
            FixturePort(shared.clone()),
            Arc::new(SystemClock),
            false,
            move |report| {
                if let Some(revision) = report.durable_revision {
                    events.revisions.lock().unwrap().push(revision);
                }
            },
        ));
        Self {
            entry,
            directory,
            vault,
            enrollment,
            shared,
        }
    }
    pub fn service(&self) -> FixtureService {
        FixtureService {
            enrollment: self.enrollment.clone(),
            namespace: self.entry.namespace(),
            inspected: None,
            authority: None,
        }
    }
    pub fn arm(&self, boundary: Boundary, account: u8) {
        *self.shared.gate.state.lock().unwrap() = GateState {
            armed: Some((boundary, account)),
            entered: false,
            released: false,
        };
    }
    pub fn wait_entered(&self) {
        let (state, _) = self
            .shared
            .gate
            .changed
            .wait_timeout_while(
                self.shared.gate.state.lock().unwrap(),
                Duration::from_secs(10),
                |state| !state.entered,
            )
            .unwrap();
        assert!(
            state.entered,
            "injected enrollment boundary was not reached"
        );
    }
    pub fn entered(&self) -> bool {
        self.shared.gate.state.lock().unwrap().entered
    }
    pub fn release(&self) {
        self.shared.gate.release();
    }
    pub fn trace(&self) -> Vec<Effect> {
        self.shared.trace.lock().unwrap().clone()
    }
    pub fn revisions(&self) -> Vec<u64> {
        self.shared.revisions.lock().unwrap().clone()
    }
    pub fn targets(&self, account: u8) -> BTreeSet<ProfileId> {
        self.directory
            .0
            .lock()
            .unwrap()
            .account_targets(&self.entry.namespace(), owner(account).account)
            .unwrap()
    }
    pub fn seeded_entries(&self) -> BTreeMap<String, Vec<u8>> {
        [1, 2]
            .into_iter()
            .map(|account| {
                let entry = self
                    .entry
                    .for_account(&owner(account).account.to_string())
                    .unwrap();
                let bytes = self
                    .vault
                    .load(&entry)
                    .unwrap()
                    .unwrap()
                    .expose()
                    .as_bytes()
                    .to_vec();
                (entry.account().to_owned(), bytes)
            })
            .collect()
    }
}
impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.release();
    }
}
