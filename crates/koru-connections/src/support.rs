// SPDX-License-Identifier: MPL-2.0

//! The scripted, in-memory stand-ins the service is verified against: a
//! vendor, the delegate's effect ledger, a clock and a credential store that
//! can be locked. Behind the `test-support` feature, which only
//! dev-dependencies turn on. Nothing here opens a socket, a file or an OS
//! keychain, and nothing here is a real vendor.
//!
//! The vendor implements the port under a qualified provider token, so no
//! test token is ever accepted by the production `CHECK`. Every secret it
//! mints contains the word `SECRET`, so a test can search any output for one.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use koru_ai_core::connections::{ProviderToken, vendors};
use koru_delegate_core::{
    AgentTurnId, AttemptGeneration, PrivateDispatcherId, PrivateEffectDisposition,
    PrivateEffectKey, PrivateEffectRefusal, PrivateEffectStage, PrivateEffectTransition,
    PrivateEffectView, PrivateOperationProposal, RunAttemptId, RunId,
};
use koru_oauth_credentials::{
    CredentialEntry, CredentialStore, Error as CredentialError, SecretString,
    SessionCredentialStore,
};
use sha2::Digest;

use crate::identity::item_identity_digest;
use crate::ports::{Clock, EffectLedger, LedgerError};
use crate::vendor::*;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the fakes did, in order, across all of them.
#[derive(Clone, Default)]
pub struct Journal(Arc<Mutex<Vec<String>>>);
impl Journal {
    pub fn push(&self, entry: impl Into<String>) {
        lock(&self.0).push(entry.into());
    }
    pub fn entries(&self) -> Vec<String> {
        lock(&self.0).clone()
    }
    pub fn clear(&self) {
        lock(&self.0).clear();
    }
}

/// A clock a test moves. `sleep` advances it and waits for nothing.
pub struct FakeClock {
    base: Instant,
    offset: Mutex<Duration>,
    slept: Mutex<Vec<Duration>>,
}
impl Default for FakeClock {
    fn default() -> Self {
        Self {
            base: Instant::now(),
            offset: Mutex::default(),
            slept: Mutex::default(),
        }
    }
}
impl FakeClock {
    pub fn advance(&self, by: Duration) {
        *lock(&self.offset) += by;
    }
    /// Every back-off the service waited, in order.
    pub fn slept(&self) -> Vec<Duration> {
        lock(&self.slept).clone()
    }
}
impl Clock for FakeClock {
    fn now(&self) -> Instant {
        self.base + *lock(&self.offset)
    }
    fn wall_ms(&self) -> i64 {
        1_000_000 + lock(&self.offset).as_millis() as i64
    }
    fn sleep(&self, duration: Duration) {
        lock(&self.slept).push(duration);
        self.advance(duration);
    }
}

/// The in-memory credential store of `koru-oauth-credentials`, with a lock a
/// test can close and a record of every slot that was touched.
#[derive(Default)]
pub struct LockableCredentials {
    inner: SessionCredentialStore,
    locked: AtomicBool,
    refuse_store: AtomicBool,
    touched: Mutex<Vec<(&'static str, String)>>,
}
impl LockableCredentials {
    /// Every call answers `Unavailable` while locked.
    pub fn set_locked(&self, locked: bool) {
        self.locked.store(locked, Ordering::SeqCst);
    }
    /// Only `store` fails: a rotated credential cannot be saved.
    pub fn set_store_refused(&self, refused: bool) {
        self.refuse_store.store(refused, Ordering::SeqCst);
    }
    /// `(operation, slot account name)` of every call, locked or not.
    pub fn touched(&self) -> Vec<(&'static str, String)> {
        lock(&self.touched).clone()
    }
    /// What a slot holds, read around the lock.
    pub fn peek(&self, entry: &CredentialEntry) -> Option<SecretString> {
        self.inner.load(entry).ok().flatten()
    }
    fn gate(
        &self,
        operation: &'static str,
        entry: &CredentialEntry,
    ) -> Result<(), CredentialError> {
        lock(&self.touched).push((operation, entry.account().to_owned()));
        if self.locked.load(Ordering::SeqCst) {
            return Err(CredentialError::Unavailable);
        }
        Ok(())
    }
}
impl CredentialStore for LockableCredentials {
    fn load(&self, entry: &CredentialEntry) -> Result<Option<SecretString>, CredentialError> {
        self.gate("load", entry)?;
        self.inner.load(entry)
    }
    fn store(&self, entry: &CredentialEntry, secret: SecretString) -> Result<(), CredentialError> {
        self.gate("store", entry)?;
        if self.refuse_store.load(Ordering::SeqCst) {
            return Err(CredentialError::Unavailable);
        }
        self.inner.store(entry, secret)
    }
    fn clear(&self, entry: &CredentialEntry) -> Result<(), CredentialError> {
        self.gate("clear", entry)?;
        self.inner.clear(entry)
    }
}

/// One of the port's operations, for scripting a fault or counting calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FakeCall {
    AuthorizationRequest,
    CompleteAuthorization,
    Refresh,
    ReadItem,
    Revoke,
}

struct Fault {
    error: VendorError,
    /// The request "left": the fake performs the effect and then fails.
    after_effect: bool,
}

struct FakeItem {
    owner: String,
    title: String,
    content: String,
}

#[derive(Default)]
struct VendorState {
    unconfigured: bool,
    revoke_unsupported: bool,
    rotate: bool,
    panic_on_read: bool,
    lifetime: Option<Duration>,
    labels: BTreeMap<String, String>,
    items: BTreeMap<String, FakeItem>,
    /// What each account allowed this client to read.
    picked: BTreeMap<String, BTreeSet<String>>,
    requests: Vec<(String, String, String)>,
    codes: HashMap<String, (String, Vec<String>)>,
    refresh: HashMap<String, String>,
    access: HashMap<String, (String, Instant)>,
    revoked: BTreeSet<String>,
    faults: HashMap<FakeCall, VecDeque<Fault>>,
    calls: Vec<FakeCall>,
    minted: Vec<String>,
    serial: u64,
    content_reads: usize,
}

type ReadHook = Box<dyn Fn() + Send + Sync>;

/// A scripted vendor: accounts with subjects and labels, picked items with
/// content, access-token lifetimes, refresh rotation, and an injected fault
/// for every [`VendorError`], including "fail after the request left".
pub struct FakeVendor {
    clock: Arc<dyn Clock>,
    journal: Journal,
    state: Mutex<VendorState>,
    read_hook: Mutex<Option<ReadHook>>,
}

pub const FAKE_ORIGIN: &str = "https://vendor.example";

impl FakeVendor {
    pub fn new(clock: Arc<dyn Clock>, journal: Journal) -> Self {
        Self {
            clock,
            journal,
            state: Mutex::default(),
            read_hook: Mutex::default(),
        }
    }
    pub fn add_account(&self, subject: &str, label: &str) {
        lock(&self.state)
            .labels
            .insert(subject.to_owned(), label.to_owned());
    }
    pub fn add_item(&self, reference: &str, owner: &str, title: &str, content: &str) {
        lock(&self.state).items.insert(
            reference.to_owned(),
            FakeItem {
                owner: owner.to_owned(),
                title: title.to_owned(),
                content: content.to_owned(),
            },
        );
    }
    /// The person signs in as `subject` and picks `picked`.
    pub fn approve(
        &self,
        consent: &AuthorizationRequest,
        subject: &str,
        picked: &[&str],
    ) -> AuthorizationCallback {
        let mut state = lock(&self.state);
        let code = mint(&mut state, "code");
        state.codes.insert(
            code.clone(),
            (
                subject.to_owned(),
                picked.iter().map(|p| (*p).to_owned()).collect(),
            ),
        );
        AuthorizationCallback {
            state: state_of(consent),
            code: SecretString::new(code).ok(),
        }
    }
    /// The person closes the consent screen.
    pub fn deny(&self, consent: &AuthorizationRequest) -> AuthorizationCallback {
        AuthorizationCallback {
            state: state_of(consent),
            code: None,
        }
    }
    /// The next call of `call` fails with `error` before doing anything.
    pub fn fail(&self, call: FakeCall, error: VendorError) {
        lock(&self.state)
            .faults
            .entry(call)
            .or_default()
            .push_back(Fault {
                error,
                after_effect: false,
            });
    }
    /// The next call of `call` does what it does and then fails with `error`.
    pub fn fail_after_send(&self, call: FakeCall, error: VendorError) {
        lock(&self.state)
            .faults
            .entry(call)
            .or_default()
            .push_back(Fault {
                error,
                after_effect: true,
            });
    }
    pub fn calls(&self, call: FakeCall) -> usize {
        lock(&self.state)
            .calls
            .iter()
            .filter(|made| **made == call)
            .count()
    }
    /// How many times content was actually produced.
    pub fn content_reads(&self) -> usize {
        lock(&self.state).content_reads
    }
    pub fn set_rotation(&self, rotate: bool) {
        lock(&self.state).rotate = rotate;
    }
    pub fn set_access_lifetime(&self, lifetime: Duration) {
        lock(&self.state).lifetime = Some(lifetime);
    }
    pub fn set_revoke_supported(&self, supported: bool) {
        lock(&self.state).revoke_unsupported = !supported;
    }
    /// A build with no registration.
    pub fn unconfigure(&self) {
        lock(&self.state).unconfigured = true;
    }
    /// The person removed the client at the vendor's own site.
    pub fn invalidate(&self, subject: &str) {
        let mut state = lock(&self.state);
        state.refresh.retain(|_, owner| owner != subject);
        state.access.retain(|_, (owner, _)| owner != subject);
    }
    /// Every access token is forgotten: the next read is unauthorized.
    pub fn drop_access_tokens(&self) {
        lock(&self.state).access.clear();
    }
    /// The item now belongs to another account.
    pub fn transfer(&self, reference: &str, new_owner: &str) {
        let mut state = lock(&self.state);
        if let Some(item) = state.items.get_mut(reference) {
            item.owner = new_owner.to_owned();
        }
        // The new owner shares it back: it stays reachable, under another
        // identity.
    }
    pub fn delete_item(&self, reference: &str) {
        lock(&self.state).items.remove(reference);
    }
    /// The account stops sharing one item with this client.
    pub fn withdraw(&self, subject: &str, reference: &str) {
        if let Some(picked) = lock(&self.state).picked.get_mut(subject) {
            picked.remove(reference);
        }
    }
    pub fn revoked(&self, subject: &str) -> bool {
        lock(&self.state).revoked.contains(subject)
    }
    /// Every secret this vendor ever issued.
    pub fn secrets(&self) -> Vec<String> {
        lock(&self.state).minted.clone()
    }
    /// Run `hook` at the start of every read, before anything is returned:
    /// how a test changes the world while a read is in flight.
    pub fn on_read(&self, hook: impl Fn() + Send + Sync + 'static) {
        *lock(&self.read_hook) = Some(Box::new(hook));
    }
    /// The next reads panic in the adapter.
    pub fn panic_on_read(&self, panic: bool) {
        lock(&self.state).panic_on_read = panic;
    }

    fn enter(&self, call: FakeCall) -> Result<Option<VendorError>, VendorError> {
        self.journal.push(format!("vendor:{call:?}"));
        let mut state = lock(&self.state);
        state.calls.push(call);
        match state.faults.get_mut(&call).and_then(VecDeque::pop_front) {
            Some(fault) if fault.after_effect => Ok(Some(fault.error)),
            Some(fault) => Err(fault.error),
            None => Ok(None),
        }
    }
}

fn mint(state: &mut VendorState, kind: &str) -> String {
    state.serial += 1;
    let secret = format!("SECRET-{kind}-{}", state.serial);
    state.minted.push(secret.clone());
    secret
}

fn state_of(consent: &AuthorizationRequest) -> String {
    consent
        .consent_url()
        .rsplit_once("state=")
        .map(|(_, state)| state.to_owned())
        .unwrap_or_default()
}

fn secret(value: String) -> SecretString {
    SecretString::new(value).unwrap_or_else(|_| unreachable!("a minted secret is never empty"))
}

impl ConnectionVendor for FakeVendor {
    fn provider(&self) -> ProviderToken {
        ProviderToken::parse(vendors::QUALIFIED[0])
            .unwrap_or_else(|| unreachable!("the qualified list is not empty"))
    }
    fn service_origin(&self) -> &str {
        FAKE_ORIGIN
    }

    fn authorization_request(
        &self,
        redirect_uri: &str,
        state: &str,
        pkce_challenge: &str,
    ) -> Result<AuthorizationRequest, VendorError> {
        if let Some(error) = self.enter(FakeCall::AuthorizationRequest)? {
            return Err(error);
        }
        let mut vendor = lock(&self.state);
        if vendor.unconfigured {
            return Err(VendorError::NotConfigured);
        }
        vendor.requests.push((
            state.to_owned(),
            pkce_challenge.to_owned(),
            redirect_uri.to_owned(),
        ));
        Ok(AuthorizationRequest::new(format!(
            "{FAKE_ORIGIN}/consent?state={state}"
        )))
    }

    fn complete_authorization(
        &self,
        callback: &AuthorizationCallback,
        pkce_verifier: &SecretString,
        redirect_uri: &str,
        _deadline: Instant,
    ) -> Result<AuthorizedAccount, VendorError> {
        let late = self.enter(FakeCall::CompleteAuthorization)?;
        let mut vendor = lock(&self.state);
        let Some(code) = &callback.code else {
            return Err(VendorError::ConsentDenied);
        };
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(pkce_verifier.expose().as_bytes()));
        let asked = vendor.requests.iter().any(|(state, expected, redirect)| {
            *state == callback.state && *expected == challenge && redirect == redirect_uri
        });
        // A code is spent by its first use, whatever comes of it.
        let Some((subject, picked)) = vendor.codes.remove(code.expose()) else {
            return Err(VendorError::Rejected);
        };
        if !asked {
            return Err(VendorError::Rejected);
        }
        let refresh = mint(&mut vendor, "refresh");
        vendor.refresh.insert(refresh.clone(), subject.clone());
        let access = mint(&mut vendor, "access");
        let lifetime = vendor.lifetime.unwrap_or(Duration::from_secs(3600));
        let expires_at = self.clock.now() + lifetime;
        vendor
            .access
            .insert(access.clone(), (subject.clone(), expires_at));
        vendor
            .picked
            .entry(subject.clone())
            .or_default()
            .extend(picked.iter().cloned());
        if let Some(error) = late {
            return Err(error);
        }
        let provider = self.provider();
        let items = picked
            .iter()
            .filter_map(|reference| {
                let item = vendor.items.get(reference)?;
                let reference = VendorItemRef::new(reference.clone())?;
                Some(PickedItem {
                    identity: item_identity_digest(provider, &reference, Some(&item.owner)),
                    title: item.title.clone(),
                    reference,
                })
            })
            .collect();
        Ok(AuthorizedAccount {
            label: vendor.labels.get(&subject).cloned().unwrap_or_default(),
            subject,
            scopes: vec!["items.read".to_owned()],
            refresh: secret(refresh),
            access: AccessToken {
                secret: secret(access),
                expires_at,
            },
            items,
        })
    }

    fn refresh(
        &self,
        refresh: &SecretString,
        _deadline: Instant,
    ) -> Result<RefreshedAccess, VendorError> {
        let late = self.enter(FakeCall::Refresh)?;
        let mut vendor = lock(&self.state);
        let Some(subject) = vendor.refresh.get(refresh.expose()).cloned() else {
            return Err(VendorError::InvalidGrant);
        };
        let access = mint(&mut vendor, "access");
        let lifetime = vendor.lifetime.unwrap_or(Duration::from_secs(3600));
        let expires_at = self.clock.now() + lifetime;
        vendor
            .access
            .insert(access.clone(), (subject.clone(), expires_at));
        let rotated = if vendor.rotate {
            vendor.refresh.remove(refresh.expose());
            let next = mint(&mut vendor, "refresh");
            vendor.refresh.insert(next.clone(), subject);
            Some(secret(next))
        } else {
            None
        };
        if let Some(error) = late {
            return Err(error);
        }
        Ok(RefreshedAccess {
            access: AccessToken {
                secret: secret(access),
                expires_at,
            },
            rotated_refresh: rotated,
        })
    }

    fn item_reference(&self, text: &str) -> Option<VendorItemRef> {
        let reference = text
            .strip_prefix(FAKE_ORIGIN)
            .and_then(|path| path.strip_prefix("/item/"))
            .unwrap_or(text);
        if reference.is_empty()
            || !reference
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return None;
        }
        VendorItemRef::new(reference)
    }

    fn read_item(
        &self,
        access: &SecretString,
        item: &VendorItemRef,
        expected_identity: &ItemIdentityDigest,
        limit_bytes: usize,
        _deadline: Instant,
    ) -> Result<ItemContent, VendorError> {
        let late = self.enter(FakeCall::ReadItem)?;
        if let Some(hook) = lock(&self.read_hook).as_ref() {
            hook();
        }
        let mut vendor = lock(&self.state);
        if vendor.panic_on_read {
            drop(vendor);
            panic!("the scripted vendor adapter panicked");
        }
        let now = self.clock.now();
        let subject = match vendor.access.get(access.expose()) {
            Some((subject, expires_at)) if *expires_at > now => subject.clone(),
            _ => return Err(VendorError::Unauthorized),
        };
        let Some(found) = vendor.items.get(item.as_str()) else {
            return Err(VendorError::NotFound);
        };
        if !vendor
            .picked
            .get(&subject)
            .is_some_and(|picked| picked.contains(item.as_str()))
        {
            return Err(VendorError::Forbidden);
        }
        // Identity first: on a mismatch no content is produced.
        if item_identity_digest(self.provider(), item, Some(&found.owner)) != *expected_identity {
            return Err(VendorError::IdentityChanged);
        }
        let total_bytes = found.content.len() as u64;
        let mut end = found.content.len().min(limit_bytes);
        while !found.content.is_char_boundary(end) {
            end -= 1;
        }
        let text = found.content[..end].to_owned();
        let truncated = end < found.content.len();
        vendor.content_reads += 1;
        if let Some(error) = late {
            return Err(error);
        }
        Ok(ItemContent {
            truncated,
            text,
            total_bytes,
        })
    }

    fn revoke(
        &self,
        refresh: &SecretString,
        _deadline: Instant,
    ) -> Result<RevokeOutcome, VendorError> {
        let late = self.enter(FakeCall::Revoke)?;
        let mut vendor = lock(&self.state);
        if vendor.revoke_unsupported {
            return Ok(RevokeOutcome::Unsupported);
        }
        let Some(subject) = vendor.refresh.get(refresh.expose()).cloned() else {
            return Err(VendorError::InvalidGrant);
        };
        // A revoke ends the whole grant of that account to this client.
        vendor.refresh.retain(|_, owner| *owner != subject);
        vendor.access.retain(|_, (owner, _)| *owner != subject);
        vendor.revoked.insert(subject);
        if let Some(error) = late {
            return Err(error);
        }
        Ok(RevokeOutcome::Revoked)
    }
}

struct LedgerRow {
    view: PrivateEffectView,
    /// Both bound barriers are still the latest and allow effects.
    barriers_current: bool,
}

#[derive(Default)]
struct LedgerState {
    rows: HashMap<(RunId, uuid::Uuid), LedgerRow>,
    /// The next advances are not answered. `true`: they commit first.
    unavailable: VecDeque<bool>,
    advances: Vec<(
        PrivateEffectTransition,
        Result<PrivateEffectStage, LedgerError>,
    )>,
}

/// The delegate's effect ledger, as far as a dispatcher sees it: the stage
/// rules of the contract, `execute` once, `deliver` once, a fenced dispatch
/// mark answered as a reply, and `stale` for a row someone else closed.
#[derive(Default)]
pub struct FakeLedger {
    journal: Journal,
    state: Mutex<LedgerState>,
}
impl FakeLedger {
    pub fn new(journal: Journal) -> Self {
        Self {
            journal,
            state: Mutex::default(),
        }
    }
    /// The delegate's first commit of an effect for `dispatcher`: a
    /// `prepared` row and the one reply that carries `execute`.
    pub fn open(
        &self,
        proposal: PrivateOperationProposal,
        dispatcher: PrivateDispatcherId,
    ) -> PrivateEffectView {
        let run = RunId::mint();
        let view = PrivateEffectView {
            key: PrivateEffectKey {
                run,
                operation_id: proposal.operation_id,
                attempt: RunAttemptId::mint(),
                generation: AttemptGeneration::FIRST,
                turn: AgentTurnId::mint(),
            },
            dispatcher,
            decision: None,
            operation: proposal,
            config_revision: 1,
            policy_revision: 1,
            routine: None,
            stage: PrivateEffectStage::Prepared,
            refusal: None,
            outcome_bytes: None,
            withheld: None,
            disposition: PrivateEffectDisposition::Execute,
            deliver: false,
        };
        let mut retained = view.clone();
        retained.disposition = PrivateEffectDisposition::ReconcileOnly;
        lock(&self.state).rows.insert(
            (run, view.operation.operation_id),
            LedgerRow {
                view: retained,
                barriers_current: true,
            },
        );
        view
    }
    /// The row as any later reply reports it: `reconcile_only`, no delivery.
    pub fn row(&self, key: &PrivateEffectKey) -> PrivateEffectView {
        lock(&self.state).rows[&(key.run, key.operation_id)]
            .view
            .clone()
    }
    /// Remove access: a grant was removed, so the bound barrier is no longer
    /// the latest. Every open row is fenced from now on.
    pub fn remove_access(&self) {
        for row in lock(&self.state).rows.values_mut() {
            row.barriers_current = false;
        }
    }
    /// The next `count` advances are not answered and commit nothing.
    pub fn unavailable(&self, count: usize) {
        lock(&self.state)
            .unavailable
            .extend(std::iter::repeat_n(false, count));
    }
    /// The delegate answers again.
    pub fn available(&self) {
        lock(&self.state).unavailable.clear();
    }
    /// The next advance commits and its reply is lost.
    pub fn lose_next_reply(&self) {
        lock(&self.state).unavailable.push_back(true);
    }
    /// Another process's dispatcher spoke for the same owner: resolution
    /// closes this row as the ledger does (`prepared` to not dispatched,
    /// `dispatched` to unknown).
    pub fn resolve_for_another_dispatcher(&self, key: &PrivateEffectKey) {
        let mut state = lock(&self.state);
        let Some(row) = state.rows.get_mut(&(key.run, key.operation_id)) else {
            return;
        };
        match row.view.stage {
            PrivateEffectStage::Prepared => {
                row.view.stage = PrivateEffectStage::Refused;
                row.view.refusal = Some(PrivateEffectRefusal::NotDispatched);
            }
            PrivateEffectStage::Dispatched => row.view.stage = PrivateEffectStage::Unknown,
            _ => {}
        }
    }
    /// Every advance that was sent, with the stage it left or why not.
    pub fn advances(
        &self,
    ) -> Vec<(
        PrivateEffectTransition,
        Result<PrivateEffectStage, LedgerError>,
    )> {
        lock(&self.state).advances.clone()
    }

    fn apply(
        row: &mut LedgerRow,
        transition: &PrivateEffectTransition,
    ) -> Result<PrivateEffectView, LedgerError> {
        use PrivateEffectStage as S;
        use PrivateEffectTransition as T;
        let view = &mut row.view;
        let mut deliver = false;
        match (*transition, view.stage) {
            (T::Dispatched {}, S::Prepared) => {
                if row.barriers_current {
                    view.stage = S::Dispatched;
                } else {
                    // A fenced dispatch mark is a reply, not a failure.
                    view.stage = S::Refused;
                    view.refusal = Some(PrivateEffectRefusal::AccessDenied);
                }
            }
            (T::Dispatched {}, S::Dispatched) => {}
            (T::Dispatched {}, S::Refused)
                if view.refusal == Some(PrivateEffectRefusal::AccessDenied) => {}
            (
                T::Confirmed {
                    outcome_bytes,
                    withheld,
                },
                S::Dispatched,
            ) => {
                view.stage = S::Confirmed;
                view.outcome_bytes = Some(outcome_bytes);
                view.withheld = Some(withheld);
                deliver = !withheld && row.barriers_current;
            }
            (
                T::Confirmed {
                    outcome_bytes,
                    withheld,
                },
                S::Confirmed,
            ) if view.outcome_bytes == Some(outcome_bytes) && view.withheld == Some(withheld) => {}
            (T::Refused { code }, S::Prepared | S::Dispatched) => {
                view.stage = S::Refused;
                view.refusal = Some(code);
            }
            (T::Refused { code }, S::Refused) if view.refusal == Some(code) => {}
            (T::Unknown {}, S::Dispatched) => view.stage = S::Unknown,
            (T::Unknown {}, S::Unknown) => {}
            _ => return Err(LedgerError::Stale),
        }
        let mut reply = view.clone();
        reply.deliver = deliver;
        Ok(reply)
    }
}
impl EffectLedger for FakeLedger {
    fn advance(
        &self,
        key: &PrivateEffectKey,
        dispatcher: PrivateDispatcherId,
        transition: &PrivateEffectTransition,
    ) -> Result<PrivateEffectView, LedgerError> {
        let mut state = lock(&self.state);
        let lost = state.unavailable.pop_front();
        let answer = if lost == Some(false) {
            Err(LedgerError::Unavailable)
        } else {
            let applied = match state.rows.get_mut(&(key.run, key.operation_id)) {
                None => Err(LedgerError::Refused),
                Some(row) if row.view.dispatcher != dispatcher || row.view.key != *key => {
                    Err(LedgerError::Stale)
                }
                Some(row) => Self::apply(row, transition),
            };
            if lost == Some(true) {
                Err(LedgerError::Unavailable)
            } else {
                applied
            }
        };
        let stage = answer.as_ref().map(|view| view.stage).map_err(|e| *e);
        state.advances.push((*transition, stage));
        self.journal.push(match &stage {
            Ok(stage) => format!("ledger:{transition:?}->{stage:?}"),
            Err(error) => format!("ledger:{transition:?}->{error:?}"),
        });
        answer
    }
}
