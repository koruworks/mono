// SPDX-License-Identifier: MPL-2.0

//! Bounded account worker and its hosted-account service.

mod cloud_status;
pub mod cloud_sync;
mod creation;
pub mod directory;
pub mod lifecycle;
mod lifecycle_ownership;
mod native;
mod profile_selection;
mod session;
mod switching;
pub mod visibility;

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use crate::delivery::{Callback, SignInDelivery};
use crate::ports::{Clock, Entropy};
use base64::Engine as _;
use koru_account_client::{AccountAuthorization, AccountClient, ClientError};
use koru_account_core::{
    AccountSessionId, AuthFlowId, AuthIdentityId, DevicePlatform, KoruIdentityId, ProfileId,
};
use koru_account_wire::{
    AccessToken, AddGoogleCandidateId, AttachProfileRequestV1, CancelAddGoogleRequestV1,
    CancelAddGoogleStatusV1, ConfirmAddGoogleRequestV1, ConfirmAddGoogleStatusV1,
    CreateProfileRequestV1, DeleteProfileRequestV1, EmailCompleteRequestV1,
    EmailCompleteResponseV1, EmailNativeStartRequestV1, EmailResendRequestV1,
    EmailResendResponseV1, EmailSignInRequestV1, EmailSignInResponseV1, EmailStartRequestV1,
    EmailVerifyRequestV1, EmailVerifyResponseV1, GoogleNativeStartRequestV1,
    LinkCallbackDeliveryV1, LinkGoogleContinueRequestV1, LinkGoogleIntentV1,
    LinkGoogleStartRequestV1, LogoutRequestV1, NativeExchangeRequestV1, OAuthState, OneUseGrant,
    PkceVerifier, ProfileAuthorizationRemovalStatusV1, ProfileSummaryV1, RebindProfileRequestV1,
    RefreshIdempotencyKey, RefreshRequestV1, RegisterDeviceRequestV1,
    RemoveProfileAuthorizationRequestV1, ResolveProfileLifecyclesRequestV1,
    RestoreProfileRequestV1, SignedDeviceDescriptorV1, TokenPairV1, UpdateAccountRequestV1,
    Version1,
};
use koru_agent_wire::{
    AccountAuthIdentityRow, AccountCommand, AccountOperationResult, AccountProfileChoice,
    AccountProfileRow, AccountStage, AccountState, AddGoogleCancellationStatus, BrowserUrl,
};
use koru_oauth_credentials::{CredentialEntry, CredentialStore, SessionCredential};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

/// The longest address any surface may hand the worker. RFC 5321's limit, and
/// the same bound the website's BFF applies before a request leaves a browser.
const MAX_EMAIL_ADDRESS_BYTES: usize = 320;

/// A bound, not the policy. The policy lives in `koru-account-core` and is the
/// server's to enforce; this only stops an absurd body leaving the app.
const MAX_PASSWORD_BYTES: usize = 512;

const COMMAND_CAPACITY: usize = 8;
const DURABILITY_RETRY: Duration = Duration::from_secs(1);
const HOSTED_RETRY_MIN: Duration = Duration::from_secs(60);
const HOSTED_RETRY_MAX: Duration = Duration::from_secs(4 * 60);
const HOSTED_RECOVERY_LIMIT: Duration = Duration::from_secs(10 * 60);

/// The hosted service gets a ten-minute recovery window. Attempts land after
/// 1, 2 and 4 minutes, then at the deadline. Capping the last delay at the
/// remaining time makes the terminal state observable at ten minutes rather
/// than after the next full exponential interval.
fn hosted_retry_delay(failures: u32, elapsed: Duration) -> Option<Duration> {
    let remaining = HOSTED_RECOVERY_LIMIT.checked_sub(elapsed)?;
    if remaining.is_zero() {
        return None;
    }
    let shift = failures.saturating_sub(1).min(6);
    Some(
        HOSTED_RETRY_MIN
            .saturating_mul(1 << shift)
            .min(HOSTED_RETRY_MAX)
            .min(remaining),
    )
}

fn credential_retry_delay(
    error: &ServiceError,
    failures: u32,
    started: &mut Option<Instant>,
    clock: &dyn Clock,
) -> Option<Duration> {
    if matches!(
        error.code,
        "credential_unavailable" | "account_directory_unavailable"
    ) {
        return Some(DURABILITY_RETRY);
    }
    if !error.retryable {
        return None;
    }
    let now = clock.monotonic();
    let first = *started.get_or_insert(now);
    hosted_retry_delay(
        failures.saturating_add(1),
        now.saturating_duration_since(first),
    )
}

pub struct Cancel(Arc<AtomicBool>);
impl Cancel {
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// A token nobody cancels, for hosts' own delivery tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn never() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

pub enum ServiceProgress {
    State(AccountState),
    Browser(BrowserUrl),
}

#[derive(Debug)]
pub struct ServiceError {
    pub code: &'static str,
    pub retryable: bool,
    pub clear_credential: bool,
}
impl ServiceError {
    pub fn retryable(code: &'static str) -> Self {
        Self {
            code,
            retryable: true,
            clear_credential: false,
        }
    }
    pub fn terminal(code: &'static str) -> Self {
        Self {
            code,
            retryable: false,
            clear_credential: false,
        }
    }
    fn revoked() -> Self {
        Self {
            code: "session_revoked",
            retryable: false,
            clear_credential: true,
        }
    }
    fn logout_incomplete(code: &'static str) -> Self {
        Self {
            code,
            retryable: false,
            clear_credential: true,
        }
    }
}

pub trait AccountService: Send + 'static {
    /// Local device policy, available before restoring or refreshing a session.
    fn cloud_profiles(
        &self,
        _account: koru_account_core::KoruIdentityId,
    ) -> Vec<koru_agent_wire::AccountProfileCloud> {
        Vec::new()
    }
    /// A fresh in-memory continuation sharing only immutable ports/journal.
    /// Called outside effects; it never restores credentials or starts Google.
    fn fork(&self) -> Option<Box<dyn AccountService>> {
        None
    }
    fn enrollment_authority(&self) -> Option<crate::enrollment::EnrollmentAuthorization> {
        None
    }
    /// Absolute deadline for an idle saved session's next access refresh.
    /// Reading it must not perform I/O or expose credential contents.
    fn access_refresh_at_millis(&self) -> Option<i64> {
        None
    }
    fn set_visibility(&mut self, _visible: Arc<AtomicBool>) {}
    fn execute(
        &mut self,
        generation: u64,
        command: AccountCommand,
        restored: Option<&SessionCredential>,
        cancel: &Cancel,
        progress: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError>;

    /// Called only after a replacement credential has reached the OS vault.
    /// A production service may retain an exact exchange request until this
    /// acknowledgement so a vault outage cannot force a new browser flow.
    fn credential_acknowledged(&mut self) {}

    /// Called after an existing credential and its directory owner have both
    /// been read back and verified from durable storage.
    fn credential_restored(
        &mut self,
        _credential: &SessionCredential,
        _record: Option<crate::identity::DirectoryRecord>,
    ) {
    }

    fn credential_owner_acknowledged(&mut self, _record: Option<crate::identity::DirectoryRecord>) {
        self.credential_acknowledged();
    }

    fn credential_removed(&mut self, _owner: Option<crate::identity::AccountOwner>) {}

    /// The random 32 bytes stored beside the refresh token this service holds,
    /// which a rotation of that token must present as its idempotency key.
    ///
    /// The worker arms it from the vault slot the token itself came out of, so
    /// the key is durable before any refresh can present it and a process that
    /// restarts mid-rotation presents the same one again. Nothing armed means
    /// no key is durable for these bytes, and `refresh` then refuses instead of
    /// performing a rotation whose replay would be impossible. It is never
    /// derived from the token: a stolen refresh token must not be enough to
    /// collect the sealed new pair from the service's retry window
    /// ([Q11](docs/features/root-catalog/QUESTIONS.md)).
    fn arm_refresh_retry_key(&mut self, _key: Option<[u8; 32]>) {}

    /// Non-mutating current-secret ownership discovery. It must not publish
    /// grants or authorize enrollment before directory/vault durability.
    fn inspect_credential(
        &mut self,
        _credential: &SessionCredential,
    ) -> Result<crate::identity::AccountOwner, ServiceError> {
        Err(ServiceError::terminal("credential_owner_unavailable"))
    }

    /// Abandons any exact hosted request owned by the cancelled generation.
    fn cancelled(&mut self, _generation: u64) {}

    /// False after a one-use proof may already have committed remotely. That
    /// phase must resolve by exact replay before cancellation/logout can erase
    /// its local credential and correlation.
    fn cancellation_safe(&self) -> bool {
        true
    }
}

pub enum CredentialChange {
    Keep,
    Replace(SessionCredential),
    Remove,
}
pub struct ServiceOutcome {
    pub state: AccountState,
    pub credential: CredentialChange,
    pub result: AccountOperationResult,
    pub follow_up: Option<AccountCommand>,
}
pub enum WorkerEvent {
    Changed,
    Browser {
        owner_scope: u64,
        requester: u64,
        generation: u64,
        attempt: u64,
        url: BrowserUrl,
    },
    Result {
        owner_scope: u64,
        requester: u64,
        generation: u64,
        result: AccountOperationResult,
    },
}
struct Envelope {
    owner_scope: u64,
    requester: u64,
    generation: u64,
    command: AccountCommand,
    cancel: Arc<AtomicBool>,
    counted: bool,
    cancel_attempt: bool,
}
struct PendingDurability {
    credential: SessionCredential,
    state: AccountState,
    result: AccountOperationResult,
    follow_up: Option<AccountCommand>,
    write: directory::PendingCredentialWrite,
}
type Active = Arc<Mutex<HashMap<u64, (u64, Arc<AtomicBool>)>>>;

pub struct AccountControl {
    sender: mpsc::SyncSender<switching::Message>,
    state: Arc<Mutex<AccountState>>,
    active: Active,
    queued: Arc<AtomicUsize>,
    next_generation: Arc<AtomicU64>,
    routing: Arc<Mutex<switching::Routing>>,
    attachments: Arc<Mutex<switching::Attachments>>,
    foreground: Arc<(Mutex<Option<bool>>, Condvar)>,
    enrollment: Option<Arc<dyn crate::enrollment::Driver>>,
    cloud: Option<Arc<dyn crate::cloud::Driver>>,
    responder_session: Option<Arc<crate::enrollment::ResponderSessionGate>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnqueueError {
    Full,
    NotOwner,
    ScopeChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountAdmission {
    pub generation: u64,
    pub owner_scope: u64,
}

impl AccountControl {
    pub fn start(
        service: impl AccountService,
        credentials: Arc<dyn CredentialStore>,
        entry: CredentialEntry,
        notify: impl Fn(WorkerEvent) + Send + 'static,
    ) -> Self {
        Self::start_with_clock(
            service,
            credentials,
            entry,
            Arc::new(crate::ports::SystemClock),
            notify,
        )
    }

    pub fn start_with_clock(
        service: impl AccountService,
        credentials: Arc<dyn CredentialStore>,
        entry: CredentialEntry,
        clock: Arc<dyn Clock>,
        notify: impl Fn(WorkerEvent) + Send + 'static,
    ) -> Self {
        Self::start_with_clock_and_foreground(service, credentials, entry, clock, true, notify)
    }

    pub fn start_with_clock_and_foreground(
        service: impl AccountService,
        credentials: Arc<dyn CredentialStore>,
        entry: CredentialEntry,
        clock: Arc<dyn Clock>,
        initial_foreground: bool,
        notify: impl Fn(WorkerEvent) + Send + 'static,
    ) -> Self {
        Self::start_with_directory(
            service,
            credentials,
            entry,
            None,
            clock,
            initial_foreground,
            notify,
        )
    }

    pub fn start_with_directory(
        service: impl AccountService,
        credentials: Arc<dyn CredentialStore>,
        entry: CredentialEntry,
        directory: Option<Arc<dyn directory::AccountDirectory>>,
        clock: Arc<dyn Clock>,
        initial_foreground: bool,
        notify: impl Fn(WorkerEvent) + Send + 'static,
    ) -> Self {
        switching::start(
            Box::new(service),
            credentials,
            entry,
            directory,
            clock,
            initial_foreground,
            notify,
        )
    }

    pub fn enqueue(&self, requester: u64, command: AccountCommand) -> Result<u64, EnqueueError> {
        self.enqueue_owned(requester, None, command)
            .map(|a| a.generation)
    }
    pub fn enqueue_owned(
        &self,
        requester: u64,
        expected_scope: Option<u64>,
        command: AccountCommand,
    ) -> Result<AccountAdmission, EnqueueError> {
        let mut routes = self.routing.lock().unwrap_or_else(|e| e.into_inner());
        let owner_scope = match &command {
            AccountCommand::Cancel { generation } => *routes
                .generations
                .get(generation)
                .ok_or(EnqueueError::NotOwner)?,
            AccountCommand::SwitchAccount { account_id } => routes
                .accounts
                .iter()
                .find_map(|(scope, account)| {
                    account
                        .filter(|a| a.to_string() == *account_id)
                        .map(|_| *scope)
                })
                .ok_or(EnqueueError::ScopeChanged)?,
            AccountCommand::AddAccount { .. } => match routes
                .anonymous
                .filter(|scope| routes.signin_scopes.contains(scope))
            {
                Some(scope) => scope,
                None => {
                    let scope = routes.next_scope;
                    routes.next_scope += 1;
                    scope
                }
            },
            _ => routes.selected,
        };
        if !matches!(
            command,
            AccountCommand::SwitchAccount { .. }
                | AccountCommand::AddAccount { .. }
                | AccountCommand::Status
        ) && expected_scope.is_some_and(|expected| expected != owner_scope)
        {
            return Err(EnqueueError::ScopeChanged);
        }
        if let AccountCommand::Cancel { generation } = command {
            match self.cancel(requester, generation) {
                None => return Err(EnqueueError::NotOwner),
                Some(false) => {
                    return Ok(AccountAdmission {
                        generation,
                        owner_scope,
                    });
                }
                Some(true) => {}
            }
            let _ = self.sender.try_send(switching::Message::Command(Envelope {
                owner_scope,
                requester,
                generation,
                command: AccountCommand::Cancel { generation },
                cancel: Arc::new(AtomicBool::new(false)),
                counted: false,
                cancel_attempt: true,
            }));
            return Ok(AccountAdmission {
                generation,
                owner_scope,
            });
        }
        let is_logout = matches!(command, AccountCommand::Logout);
        let is_add = matches!(command, AccountCommand::AddAccount { .. });
        let anonymous_signin = is_add
            || (matches!(
                command,
                AccountCommand::StartGoogle { .. }
                    | AccountCommand::StartApple { .. }
                    | AccountCommand::ContinueWithEmail { .. }
            ) && routes
                .accounts
                .get(&owner_scope)
                .is_none_or(|account| account.is_none()));
        let admission_limit = COMMAND_CAPACITY + usize::from(is_logout);
        if self.queued.fetch_add(1, Ordering::AcqRel) >= admission_limit {
            self.queued.fetch_sub(1, Ordering::AcqRel);
            return Err(EnqueueError::Full);
        }
        let generation = self.next_generation.fetch_add(1, Ordering::AcqRel);
        let cancel = Arc::new(AtomicBool::new(false));
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(generation, (requester, cancel.clone()));
        routes.generations.insert(generation, owner_scope);
        routes.queued_generations.insert(generation);
        if self
            .sender
            .try_send(switching::Message::Command(Envelope {
                owner_scope,
                requester,
                generation,
                command,
                cancel,
                counted: true,
                cancel_attempt: false,
            }))
            .is_err()
        {
            self.active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&generation);
            routes.generations.remove(&generation);
            routes.queued_generations.remove(&generation);
            self.queued.fetch_sub(1, Ordering::AcqRel);
            return Err(EnqueueError::Full);
        }
        if anonymous_signin {
            routes.signin_scopes.insert(owner_scope);
            routes.anonymous = Some(owner_scope);
            routes.accounts.entry(owner_scope).or_insert(None);
        }
        // Only admitted cleanup may retire authority. The routing lock still
        // fences coordinator publication while these cancellation flags change.
        if is_logout {
            routes.retiring_scopes.insert(owner_scope);
            for (owned_generation, (_, flag)) in
                self.active.lock().unwrap_or_else(|e| e.into_inner()).iter()
            {
                if *owned_generation != generation
                    && routes.generations.get(owned_generation) == Some(&owner_scope)
                {
                    flag.store(true, Ordering::Release);
                }
            }
            if let Some(account) = routes.accounts.get(&owner_scope).and_then(|a| *a) {
                let cutoff = routes.next_scope.saturating_sub(1);
                routes
                    .retired_accounts
                    .entry(account)
                    .and_modify(|old| *old = (*old).max(cutoff))
                    .or_insert(cutoff);
                let owned = routes
                    .accounts
                    .iter()
                    .filter_map(|(scope, value)| (*value == Some(account)).then_some(*scope))
                    .collect::<Vec<_>>();
                routes.retiring_scopes.extend(owned);
                if let Some(gate) = &self
                    .attachments
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .responder
                {
                    gate.disable_account(account);
                }
            }
        }
        Ok(AccountAdmission {
            generation,
            owner_scope,
        })
    }
    fn cancel(&self, requester: u64, generation: u64) -> Option<bool> {
        if let Some((owner, flag)) = self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&generation)
        {
            if *owner == requester {
                return Some(
                    flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok(),
                );
            }
        }
        None
    }
    pub fn disconnect(&self, requester: u64) {
        let generations = self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(|(generation, (owner, flag))| {
                (*owner == requester
                    && flag
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok())
                .then_some(*generation)
            })
            .collect::<Vec<_>>();
        for generation in generations {
            let owner_scope = self
                .routing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .generations
                .get(&generation)
                .copied()
                .unwrap_or(0);
            let _ = self.sender.try_send(switching::Message::Command(Envelope {
                owner_scope,
                requester,
                generation,
                command: AccountCommand::Cancel { generation },
                cancel: Arc::new(AtomicBool::new(false)),
                counted: false,
                cancel_attempt: true,
            }));
        }
    }
    pub fn set_enrollment_control(&mut self, enrollment: Arc<dyn crate::enrollment::Driver>) {
        enrollment.set_foreground(
            self.foreground
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .unwrap_or(false),
        );
        self.enrollment = Some(enrollment);
        self.attachments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .enrollment = self.enrollment.clone();
        let _ = self.sender.try_send(switching::Message::Wake);
    }

    /// Attach the Cloud coordinator to the same routing that owns enrollment.
    /// It is activated, refreshed and deactivated by the same decisions.
    pub fn set_cloud_control(&mut self, cloud: Arc<dyn crate::cloud::Driver>) {
        cloud.set_foreground(
            self.foreground
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .unwrap_or(false),
        );
        self.cloud = Some(cloud);
        self.attachments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cloud = self.cloud.clone();
        let _ = self.sender.try_send(switching::Message::Wake);
    }

    pub fn set_responder_session_gate(
        &mut self,
        session: Arc<crate::enrollment::ResponderSessionGate>,
    ) {
        self.responder_session = Some(session);
        self.attachments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .responder = self.responder_session.clone();
        let _ = self.sender.try_send(switching::Message::Wake);
    }

    pub fn wake_enrollment(&self) {
        if let Some(enrollment) = &self.enrollment {
            enrollment.wake();
        }
    }

    /// A durable local commit, a remote hint or a host resume. The pass is
    /// rescheduled behind the coordinator's trailing debounce, never inline.
    pub fn wake_cloud(&self) {
        if let Some(cloud) = &self.cloud {
            cloud.wake();
        }
    }

    /// Pause at the next worker operation boundary. An in-flight hosted call
    /// keeps its exact request and finishes resolving normally; resume does not
    /// create another worker or discard pending vault acknowledgements.
    pub fn set_foreground(&self, foreground: bool) {
        *self.foreground.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(foreground);
        if foreground {
            self.foreground.1.notify_all();
        }
        if let Some(enrollment) = &self.enrollment {
            enrollment.set_foreground(foreground);
        }
        if let Some(cloud) = &self.cloud {
            cloud.set_foreground(foreground);
        }
        let _ = self.sender.try_send(switching::Message::Wake);
    }

    /// Existing operation ownership, including queued and retry-held envelopes.
    /// Read before state when projecting a finished-without-result observation.
    pub fn requester_generation_active(&self, requester: u64, generation: u64) -> bool {
        is_active(&self.active, requester, generation)
    }

    pub fn requester_active(&self, requester: u64) -> bool {
        let routes = self.routing.lock().unwrap_or_else(|e| e.into_inner());
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(generation, (owner, _))| {
                *owner == requester && routes.generations.get(generation) == Some(&routes.selected)
            })
    }
    pub fn requester_scope_active(&self, requester: u64, scope: u64) -> bool {
        let routes = self.routing.lock().unwrap_or_else(|e| e.into_inner());
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(generation, (owner, _))| {
                *owner == requester && routes.generations.get(generation) == Some(&scope)
            })
    }

    /// Commands admitted but not yet taken by a worker, across every scope.
    ///
    /// This is the number `enqueue` checks against [`COMMAND_CAPACITY`], so it
    /// is what a *background* caller should read before adding to it: a
    /// periodic nudge must never spend the admission slot a person's next
    /// command needs. It is not the same as [`AccountControl::state`]'s
    /// `queue_depth`, which counts only the selected scope's waiting work.
    pub fn queued_commands(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }

    pub fn state(&self) -> AccountState {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        {
            let routes = self.routing.lock().unwrap_or_else(|e| e.into_inner());
            state.queue_depth = routes
                .queued_generations
                .iter()
                .filter(|g| routes.generations.get(g) == Some(&routes.selected))
                .count();
        }
        if let Some(enrollment) = &self.enrollment {
            let snapshot = enrollment.snapshot();
            if snapshot.owner_scope == Some(state.active_scope)
                && snapshot.account_id == state.active_account_id
            {
                state.enrollment = snapshot.report.state;
                state.enrollment_failure_code = snapshot.report.failure_code.map(str::to_owned);
                state.enrollment_retry_at_millis = snapshot.report.retry_at_millis;
                state.enrollments = snapshot
                    .profiles
                    .iter()
                    .map(crate::enrollment::ProfileReport::wire)
                    .collect();
            }
        }
        if let Some(cloud) = &self.cloud {
            cloud_status::project(&mut state, &cloud.snapshot());
        } else {
            for profile in &mut state.cloud_profiles {
                profile.stage = koru_agent_wire::CloudConnectionStage::Retrying;
            }
        }
        state
    }
}

impl Drop for AccountControl {
    fn drop(&mut self) {
        // Destruction is not a foreground transition: a worker parked before
        // its first restore must exit without touching the vault or network.
        *self.foreground.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.foreground.1.notify_all();
        let _ = self.sender.try_send(switching::Message::Wake);
    }
}

fn finish(active: &Active, generation: u64) {
    active
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&generation);
}
fn is_active(active: &Active, requester: u64, generation: u64) -> bool {
    active
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&generation)
        .is_some_and(|(owner, _)| *owner == requester)
}
fn publish_cancelled(
    envelope: &Envelope,
    state: &Mutex<AccountState>,
    notify: &(impl Fn(WorkerEvent) + ?Sized),
) {
    let mut next = state.lock().unwrap_or_else(|e| e.into_inner()).clone();
    next.generation = envelope.generation;
    next.stage = AccountStage::Cancelled;
    next.capability = true;
    next.retry_at_millis = None;
    next.failure_code = Some("cancelled".into());
    publish_state(next, state, notify);
    if envelope.requester != 0 {
        notify(WorkerEvent::Result {
            owner_scope: envelope.owner_scope,
            requester: envelope.requester,
            generation: envelope.generation,
            result: command_failure(&envelope.command, "cancelled"),
        });
    }
}
/// A refusal the worker raised before or instead of the command's own answer.
///
/// `InspectCloudSubscription` keeps its shape because `current: None` says
/// exactly the true thing — the card decision is unknown.
///
/// The two Cloud sync commands deliberately do **not** get the same treatment.
/// Their shape is a list of Profiles, and an empty list already means
/// something: no Profile has Cloud sync state on this device. Answering a
/// failure with an empty list would tell a person every Profile is switched
/// off, which is the one wrong thing to say about a switch. So a failure here
/// is `Failed { code }`, and the handler answers `CloudSync` — with each
/// Profile's own `error` — for everything it can reach the Profile set for. A
/// `Failed` from these two therefore always means "nothing is known": no host
/// port, no credential, no account owner, or the local inventory and the
/// remembered state could not be read.
fn command_failure(command: &AccountCommand, code: &str) -> AccountOperationResult {
    match command {
        AccountCommand::InspectCloudSubscription { request_id } => {
            AccountOperationResult::CloudSubscription {
                request_id: *request_id,
                current: None,
                access: None,
            }
        }
        _ if code == "cancelled" => AccountOperationResult::Cancelled,
        _ => AccountOperationResult::Failed { code: code.into() },
    }
}
fn signed_out(generation: u64) -> AccountState {
    AccountState {
        generation,
        stage: AccountStage::SignedOut,
        capability: true,
        ..Default::default()
    }
}
fn publish_state(
    state: AccountState,
    slot: &Mutex<AccountState>,
    notify: &(impl Fn(WorkerEvent) + ?Sized),
) {
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = state;
    notify(WorkerEvent::Changed);
}
fn failure_at(
    generation: u64,
    code: &str,
    retry: Option<Duration>,
    state: &Mutex<AccountState>,
    notify: &(impl Fn(WorkerEvent) + ?Sized),
    clock: &dyn Clock,
) {
    let failure = failed_state(generation, code, retry, clock);
    let mut next = state.lock().unwrap_or_else(|e| e.into_inner()).clone();
    next.generation = failure.generation;
    next.stage = failure.stage;
    next.capability = failure.capability;
    next.retry_at_millis = failure.retry_at_millis;
    next.failure_code = failure.failure_code;
    publish_state(next, state, notify);
}
fn failed_state(
    generation: u64,
    code: &str,
    retry: Option<Duration>,
    clock: &dyn Clock,
) -> AccountState {
    let retry_at_millis = retry.and_then(|delay| {
        clock
            .now_millis()?
            .checked_add(i64::try_from(delay.as_millis()).ok()?)
    });
    AccountState {
        generation,
        stage: AccountStage::Failed,
        capability: true,
        retry_at_millis,
        failure_code: Some(code.into()),
        ..Default::default()
    }
}
pub fn startup_capability(
    origin: Option<&str>,
    descriptor: Option<&SignedDeviceDescriptorV1>,
) -> bool {
    origin.is_some_and(|origin| !origin.is_empty()) && descriptor.is_some()
}

use crate::enrollment::Driver as EnrollmentAccountControl;

/// Required host dependencies; there is deliberately no default local inventory.
pub struct AccountPorts {
    pub delivery: Arc<dyn SignInDelivery>,
    pub local_profiles: Arc<dyn lifecycle::LocalProfileInventory>,
    pub lifecycle_repository: lifecycle::LifecycleRepository,
    pub clock: Arc<dyn Clock>,
    pub entropy: Arc<dyn Entropy>,
    /// Optional on purpose. A host without a local Workspace catalog and a
    /// place to remember the choice — Android today — keeps working and
    /// answers `cloud_sync_unavailable` to the Cloud sync commands.
    pub cloud_sync: Option<Arc<dyn cloud_sync::CloudSyncPort>>,
}

pub struct ProductionAccountService {
    delivery: Arc<dyn SignInDelivery>,
    clock: Arc<dyn Clock>,
    entropy: Arc<dyn Entropy>,
    client: AccountClient,
    signing_key: Arc<koru_device::DeviceSigningKey>,
    descriptor: SignedDeviceDescriptorV1,
    platform: DevicePlatform,
    app_version: String,
    access: Option<AccessToken>,
    access_expires_at_millis: Option<i64>,
    profiles: Vec<ProfileSummaryV1>,
    pending_exchange: Option<Pending<NativeExchangeRequestV1>>,
    pending_native: Option<native::PendingNative>,
    pending_creation: Option<creation::PendingCreation>,
    /// An email journey the app is part-way through: the flow, its correlation
    /// value and its PKCE verifier, which bind this device to the account being
    /// created and must never leave this process.
    pending_email: Option<PendingEmail>,
    pending_logout: bool,
    pending_link: Option<Pending<LinkGoogleContinueRequestV1>>,
    pending_rebind: Option<Pending<RebindProfileRequestV1>>,
    enrollment: Option<Arc<dyn EnrollmentAccountControl>>,
    /// The same verified authorization reaches the Cloud coordinator at every
    /// moment it reaches enrollment. There is one account authorization on a
    /// device, so there is one value pushed to both.
    cloud: Option<Arc<dyn crate::cloud::Driver>>,
    responder_session: Option<Arc<crate::enrollment::ResponderSessionGate>>,
    local_profiles: Arc<dyn lifecycle::LocalProfileInventory>,
    cloud_sync: Option<Arc<dyn cloud_sync::CloudSyncPort>>,
    /// Whether an account has Koru Cloud, as last answered. Memory only, and
    /// read by one thing: a Profile waiting to start syncing
    /// (`cloud_sync::cloud_access_known`).
    cloud_access: Option<(KoruIdentityId, bool)>,
    /// When the service last accepted this sign-in: set with every token
    /// pair it answers, published as `AccountState::session_checked_at_millis`.
    session_checked_at_millis: Option<i64>,
    /// The Profiles whose Store this process has already opened to Cloud, so
    /// the reconcile opens each once rather than on every heartbeat. Memory
    /// only: opening is safe to repeat, and a new process asks again.
    cloud_opened: std::collections::BTreeSet<ProfileId>,
    lifecycle_repository: Arc<Mutex<lifecycle::LifecycleRepository>>,
    lifecycle_ownership: Arc<Mutex<lifecycle_ownership::Ownership>>,
    directory_namespace: Option<String>,
    durable_owner: Option<crate::identity::AccountOwner>,
    durable_record_version: Option<i64>,
    unacknowledged_session_id: Option<String>,
    removed_credential_owner: Option<crate::identity::AccountOwner>,
    durable_session_id: Option<String>,
    /// Armed from the vault slot that holds the refresh token; see
    /// [`AccountService::arm_refresh_retry_key`].
    refresh_retry_key: Option<[u8; 32]>,
    /// A rotation has been answered and the hosted device registration it may
    /// owe has not run yet. It runs once the new pair is durable, never before.
    device_registration_due: bool,
    visible: Option<Arc<AtomicBool>>,
}

struct Pending<T> {
    command: AccountCommand,
    request: T,
}

/// Everything an in-app email journey carries between its steps.
///
/// `client_state` and `verifier` are the two halves of the native binding: the
/// exchange proves the app holds both, which is what stops a grant lifted in
/// transit being redeemed anywhere else. None of it leaves this process.
struct PendingEmail {
    flow_id: AuthFlowId,
    client_state: String,
    verifier: String,
    address: String,
    /// Held between the code screen and the password screen, and nowhere else.
    ///
    /// `email_complete` re-checks the code server-side so that a password can
    /// only be set by somebody who still holds it. With the journey inside the
    /// app, this is the only place it can live for those few seconds.
    code: Option<String>,
}

impl Drop for PendingEmail {
    fn drop(&mut self) {
        self.client_state.zeroize();
        self.verifier.zeroize();
        if let Some(code) = self.code.as_mut() {
            code.zeroize();
        }
    }
}
impl ProductionAccountService {
    fn journal(&self) -> std::sync::MutexGuard<'_, lifecycle::LifecycleRepository> {
        self.lifecycle_repository
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
    pub fn new(
        client: AccountClient,
        descriptor: SignedDeviceDescriptorV1,
        signing_key: koru_device::DeviceSigningKey,
        platform: DevicePlatform,
        app_version: String,
        ports: AccountPorts,
    ) -> Self {
        Self {
            client,
            descriptor,
            signing_key: Arc::new(signing_key),
            platform,
            app_version,
            access: None,
            access_expires_at_millis: None,
            profiles: Vec::new(),
            pending_exchange: None,
            pending_native: None,
            pending_creation: None,
            pending_email: None,
            pending_logout: false,
            pending_link: None,
            pending_rebind: None,
            enrollment: None,
            cloud: None,
            responder_session: None,
            local_profiles: ports.local_profiles,
            cloud_sync: ports.cloud_sync,
            cloud_access: None,
            session_checked_at_millis: None,
            cloud_opened: std::collections::BTreeSet::new(),
            lifecycle_repository: Arc::new(Mutex::new(ports.lifecycle_repository)),
            lifecycle_ownership: Arc::new(Mutex::new(lifecycle_ownership::Ownership::default())),
            delivery: ports.delivery,
            clock: ports.clock,
            entropy: ports.entropy,
            directory_namespace: None,
            durable_owner: None,
            durable_record_version: None,
            unacknowledged_session_id: None,
            removed_credential_owner: None,
            durable_session_id: None,
            refresh_retry_key: None,
            device_registration_due: false,
            visible: None,
        }
    }

    pub fn with_enrollment(mut self, enrollment: Arc<dyn crate::enrollment::Driver>) -> Self {
        self.enrollment = Some(enrollment);
        self
    }

    /// Attach the portable Cloud coordinator. It receives exactly the
    /// [`crate::enrollment::EnrollmentAuthorization`] enrollment receives, at
    /// the same moments, so the two can never disagree about who is signed in.
    pub fn with_cloud(mut self, cloud: Arc<dyn crate::cloud::Driver>) -> Self {
        self.cloud = Some(cloud);
        self
    }

    pub fn with_directory_namespace(mut self, namespace: String) -> Self {
        self.directory_namespace = Some(namespace);
        self
    }

    fn enrollment_authorization(&self) -> Option<crate::enrollment::EnrollmentAuthorization> {
        // Issuance may have replaced access while the previous durable owner
        // still survives. Never combine those two different acknowledgements.
        if self.unacknowledged_session_id.is_some() {
            return None;
        }
        Some(crate::enrollment::EnrollmentAuthorization {
            access: self.access.as_ref()?.clone(),
            owner: self.durable_owner?,
            record_version: self.durable_record_version?,
            namespace: self.directory_namespace.as_ref()?.clone(),
            session_id: self.durable_session_id.as_ref()?.clone(),
        })
    }

    pub fn with_responder_session(
        mut self,
        session: Arc<crate::enrollment::ResponderSessionGate>,
    ) -> Self {
        self.responder_session = Some(session);
        self
    }

    #[cfg(test)]
    fn with_enrollment_driver(mut self, enrollment: Arc<dyn EnrollmentAccountControl>) -> Self {
        self.enrollment = Some(enrollment);
        self
    }

    #[cfg(test)]
    fn with_cloud_sync_port(mut self, port: Arc<dyn cloud_sync::CloudSyncPort>) -> Self {
        self.cloud_sync = Some(port);
        self
    }

    #[cfg(test)]
    fn with_local_profile_inventory(
        mut self,
        inventory: Arc<dyn lifecycle::LocalProfileInventory>,
    ) -> Self {
        self.local_profiles = inventory;
        self
    }

    fn state(&self, generation: u64, stage: AccountStage) -> AccountState {
        AccountState {
            generation,
            stage,
            capability: true,
            profile_count: self.profiles.len(),
            cloud_profiles: self
                .durable_owner
                .map(|owner| self.cloud_profiles(owner.account))
                .unwrap_or_default(),
            // Only while there is a session to speak of.
            session_checked_at_millis: self.access.as_ref().and(self.session_checked_at_millis),
            ..Default::default()
        }
    }
    fn authorization(&self) -> Result<AccountAuthorization<'_>, ServiceError> {
        self.access
            .as_ref()
            .map(AccountAuthorization::Bearer)
            .ok_or_else(|| ServiceError::terminal("authentication_required"))
    }
    fn access_is_fresh(&self) -> bool {
        let Some(expiry) = self.access_expires_at_millis else {
            return false;
        };
        let Some(now) = self.clock.now_millis() else {
            return false;
        };
        now.checked_add(60_000).is_some_and(|slack| slack < expiry) && self.access.is_some()
    }
    fn accept_tokens(&mut self, tokens: TokenPairV1) -> Result<SessionCredential, ServiceError> {
        let access_expires_at_millis = tokens.access_expires_at_millis;
        let credential =
            SessionCredential::new(tokens.session_id.to_string(), tokens.refresh_token.0)
                .map_err(|_| ServiceError::terminal("credential_invalid"))?;
        self.access = Some(tokens.access_token);
        self.access_expires_at_millis = Some(access_expires_at_millis);
        // The service has just accepted this sign-in.
        self.session_checked_at_millis = self.clock.now_millis();
        self.unacknowledged_session_id = Some(credential.session_id.clone());
        // Access remains private to this operation until the exact credential
        // and its authenticated directory owner have both acknowledged.
        Ok(credential)
    }
    fn finish_exchange(&mut self, generation: u64) -> Result<ServiceOutcome, ServiceError> {
        let mut request = self
            .pending_exchange
            .as_ref()
            .map(|pending| pending.request.clone())
            .ok_or_else(|| ServiceError::terminal("authentication_failed"))?;
        if request.device_ownership_proof.is_none() {
            let response = match self.client.native_device_challenge(&request) {
                Err(ClientError::Api { code, .. })
                    if self.pending_creation.is_some()
                        && code.as_str() == koru_account_wire::ErrorCode::FLOW_EXPIRED =>
                {
                    return Ok(self.expired_creation(generation));
                }
                value => value.map_err(map_client_error)?,
            };
            if response.challenge.len() > 16 * 1024 {
                return Err(ServiceError::terminal("authentication_failed"));
            }
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&response.challenge)
                .map_err(|_| ServiceError::terminal("authentication_failed"))?;
            let challenge: koru_account_core::DeviceOwnershipChallenge =
                serde_json::from_slice(&bytes)
                    .map_err(|_| ServiceError::terminal("authentication_failed"))?;
            let expected = koru_device::DeviceDescriptorV1::try_from(self.descriptor.clone())
                .map_err(|_| ServiceError::terminal("authentication_failed"))?;
            let parent_flow_id = request
                .grant
                .0
                .split_once('.')
                .and_then(|(flow, _)| flow.parse::<koru_account_core::AuthFlowId>().ok())
                .ok_or_else(|| ServiceError::terminal("authentication_failed"))?;
            if challenge.parent_flow_id != Some(parent_flow_id)
                || challenge.device.descriptor != expected
                || challenge.device.registration.platform != self.platform
                || challenge.device.registration.app_version != self.app_version
                || challenge.device.registration.enrollment_protocol_version
                    != Some(koru_pairing::ENROLLMENT_VERSION)
                || challenge.device.registration.peer_protocol_version
                    != Some(koru_sync_control::PEER_PROTOCOL_VERSION)
            {
                return Err(ServiceError::terminal("authentication_failed"));
            }
            let proof = challenge.sign(&self.signing_key);
            request.device_ownership_proof = Some(
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                    serde_json::to_vec(&proof)
                        .map_err(|_| ServiceError::terminal("authentication_failed"))?,
                ),
            );
            // Store signed evidence before an exchange can commit remotely.
            self.pending_exchange
                .as_mut()
                .expect("pending exchange")
                .request = request.clone();
        }
        let exchange = match self.client.native_exchange(&request) {
            Ok(exchange) => exchange,
            Err(ClientError::Api { code, .. })
                if self.pending_creation.is_some()
                    && code.as_str() == koru_account_wire::ErrorCode::FLOW_EXPIRED =>
            {
                return Ok(self.expired_creation(generation));
            }
            Err(error) => {
                let mapped = map_client_error(error);
                if !mapped.retryable {
                    self.pending_exchange = None;
                }
                return Err(mapped);
            }
        };
        // Retain the exact request until the worker acknowledges the rotated
        // credential in the OS vault. A vault outage can then retry the sealed
        // hosted result without another listener, browser, state or verifier.
        let credential = self.accept_tokens(exchange.tokens)?;
        if let Some(creation) = &mut self.pending_creation {
            creation.issued_session_id = Some(credential.session_id.clone());
        }
        Ok(ServiceOutcome {
            state: self.state(generation, AccountStage::SignedIn),
            credential: CredentialChange::Replace(credential),
            result: AccountOperationResult::SignedIn,
            follow_up: None,
        })
    }
    fn finish_link(&mut self, generation: u64) -> Result<ServiceOutcome, ServiceError> {
        let request = self
            .pending_link
            .as_ref()
            .map(|pending| pending.request.clone())
            .ok_or_else(|| ServiceError::terminal("authentication_failed"))?;
        let candidate = match self
            .client
            .link_google_continue(&request, &self.authorization()?)
        {
            Ok(candidate) => {
                self.pending_link = None;
                candidate
            }
            Err(error) => {
                let mapped = map_client_error(error);
                if !mapped.retryable {
                    self.pending_link = None;
                }
                return Err(mapped);
            }
        };
        Ok(ServiceOutcome {
            state: self.state(generation, AccountStage::SignedIn),
            credential: CredentialChange::Keep,
            result: AccountOperationResult::AddGoogleCandidate {
                candidate_id: candidate.candidate_id.0,
                profile_id: candidate.profile_id.to_string(),
                verified_email: candidate.verified_email,
                expires_at_millis: candidate.expires_at_millis,
            },
            follow_up: None,
        })
    }
    fn finish_rebind(&mut self) -> Result<ProfileSummaryV1, ServiceError> {
        let request = self
            .pending_rebind
            .as_ref()
            .map(|pending| pending.request.clone())
            .ok_or_else(|| ServiceError::terminal("authentication_failed"))?;
        let account = self
            .durable_owner
            .ok_or_else(|| ServiceError::terminal("credential_owner_unavailable"))?
            .account;
        let old_operation = self
            .journal()
            .get(request.profile_id)
            .map_err(|error| ServiceError::retryable(error.code))?
            .map(|record| record.operation);
        match self.client.rebind_profile(&request, &self.authorization()?) {
            Ok(profile) => {
                if profile.profile_id != request.profile_id
                    || profile.koru_identity_id != account
                    || account == request.erased_koru_identity_id
                {
                    return Err(ServiceError::terminal(
                        "profile_lifecycle_identity_mismatch",
                    ));
                }
                self.lifecycle_ownership
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .proven_rebind(profile.profile_id, account);
                self.journal()
                    .retire_for_rebind(profile.profile_id, old_operation)
                    .map_err(|error| ServiceError::retryable(error.code))?;
                self.pending_rebind = None;
                Ok(profile)
            }
            Err(error) => {
                let mapped = map_client_error(error);
                if !mapped.retryable {
                    self.pending_rebind = None;
                }
                Err(mapped)
            }
        }
    }
    fn remember_profile(&mut self, profile: &ProfileSummaryV1) {
        self.profiles
            .retain(|known| known.profile_id != profile.profile_id);
        self.profiles.push(profile.clone());
    }
    fn remote_logout(&mut self, restored: Option<&SessionCredential>) -> Result<(), ServiceError> {
        let issued = self.unacknowledged_session_id.clone().or_else(|| {
            self.pending_creation
                .as_ref()
                .and_then(|creation| creation.issued_session_id.clone())
        });
        if issued.is_none() {
            self.resolve_native_for_logout()?;
        }
        let mut session_id = issued
            .clone()
            .or_else(|| restored.map(|record| record.session_id.clone()));
        if issued.is_none()
            && let Some(pending) = self.pending_exchange.as_ref()
        {
            let exchange = self
                .client
                .native_exchange(&pending.request)
                .map_err(map_client_error)?;
            let credential = self.accept_tokens(exchange.tokens)?;
            session_id = Some(credential.session_id.clone());
            self.pending_exchange = None;
        }
        if self.pending_link.is_some() || self.pending_rebind.is_some() {
            if !self.access_is_fresh() {
                let restored = restored.ok_or_else(ServiceError::revoked)?;
                let _ = self.refresh(restored)?;
            }
            if self.pending_link.is_some() {
                let _ = self.finish_link(0)?;
            }
            if self.pending_rebind.is_some() {
                let _ = self.finish_rebind()?;
            }
        }
        if let (Some(access), Some(session_id)) = (&self.access, session_id)
            && let Ok(session_id) = AccountSessionId::from_str(&session_id)
        {
            self.client
                .logout(
                    &LogoutRequestV1 {
                        version: Version1,
                        session_id,
                    },
                    &AccountAuthorization::Bearer(access),
                )
                .map_err(map_client_error)?;
        }
        Ok(())
    }
    fn refresh(&mut self, restored: &SessionCredential) -> Result<SessionCredential, ServiceError> {
        let session_id = AccountSessionId::from_str(&restored.session_id)
            .map_err(|_| ServiceError::terminal("credential_invalid"))?;
        // The key is random bytes the vault already holds beside the token
        // being presented, not a value derived from that token: holding the
        // token must not be enough to collect the sealed new pair out of the
        // service's retry window. Because it is durable before the request
        // goes out, a rotation this process loses can be replayed under the
        // same key by the next one — which is the only thing that saves a
        // rotation the service has already performed. Without a durable key
        // there is no replay, so the request is not sent at all.
        let retry_key = self
            .refresh_retry_key
            .ok_or_else(|| ServiceError::retryable("credential_unavailable"))?;
        let response = match self.client.refresh(&RefreshRequestV1 {
            version: Version1,
            session_id,
            refresh_token: koru_account_wire::RefreshToken(restored.refresh_token.clone()),
            idempotency_key: RefreshIdempotencyKey::from_bytes(retry_key),
        }) {
            Ok(response) => response,
            Err(error) if refresh_was_revoked(&error) => {
                self.access = None;
                if let (Some(session), Some(owner), Some(id)) = (
                    &self.responder_session,
                    self.durable_owner,
                    &self.durable_session_id,
                ) {
                    session.disable_account_session(owner.account, id);
                }
                return Err(ServiceError::revoked());
            }
            Err(error) => return Err(map_client_error(error)),
        };
        // Nothing hosted may run between here and the vault write. The device
        // registration refresh this rotation may owe — a devices call, and
        // sometimes a challenge and a registration, measured at the bulk of
        // 5.3 seconds — used to run on this line with `?`, so a stop or a
        // failure threw away a pair the service had already rotated to and
        // made every later launch present a superseded token. It is now owed
        // until the new pair is durable, and `credential_owner_acknowledged`
        // pays it.
        self.device_registration_due = true;
        let access_expires_at_millis = response.access_expires_at_millis;
        let credential = self.accept_tokens(response)?;
        // The server keeps only the newest pair current. Paired with the
        // "persisted" line, this says whether a rotation reached the vault
        // before the process stopped. A code and a time, never a value.
        tracing::info!(
            target: "koru_account",
            rotated = credential.refresh_token != restored.refresh_token,
            access_expires_at_millis,
            "credential refresh answered"
        );
        Ok(credential)
    }

    /// A saved native session outlives the build that created it. Refresh the
    /// hosted registration when an app update changes its advertised wire
    /// versions; otherwise candidate discovery keeps returning peers for the
    /// old protocol and every healthy device looks offline until the user
    /// signs in again.
    fn refresh_device_registration(&self, access: &AccessToken) -> Result<(), ServiceError> {
        let Some(owner) = self.durable_owner else {
            // Fresh exchanges register the device as part of their atomic
            // commit. This path is only for an already acknowledged session.
            return Ok(());
        };
        let authorization = AccountAuthorization::Bearer(access);
        let devices = self
            .client
            .devices(&authorization)
            .map_err(map_client_error)?;
        let current_enrollment = Some(koru_pairing::ENROLLMENT_VERSION);
        let current_peer = Some(koru_sync_control::PEER_PROTOCOL_VERSION);
        if devices.devices.iter().any(|device| {
            device.device_id == self.descriptor.device_id
                && !device.revoked
                && device.display_name == self.descriptor.display_name
                && device.platform == self.platform
                && device.app_version == self.app_version
                && device.enrollment_protocol_version == current_enrollment
                && device.peer_protocol_version == current_peer
        }) {
            return Ok(());
        }

        let mut request = RegisterDeviceRequestV1 {
            device_ownership_proof: None,
            version: Version1,
            descriptor: self.descriptor.clone(),
            platform: self.platform,
            app_version: self.app_version.clone(),
            enrollment_protocol_version: current_enrollment,
            peer_protocol_version: current_peer,
        };
        let response = self
            .client
            .device_registration_challenge(&request, &authorization)
            .map_err(map_client_error)?;
        if response.challenge.len() > 16 * 1024 {
            return Err(ServiceError::terminal("device_registration_invalid"));
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&response.challenge)
            .map_err(|_| ServiceError::terminal("device_registration_invalid"))?;
        let challenge: koru_account_core::DeviceOwnershipChallenge = serde_json::from_slice(&bytes)
            .map_err(|_| ServiceError::terminal("device_registration_invalid"))?;
        let expected = koru_device::DeviceDescriptorV1::try_from(self.descriptor.clone())
            .map_err(|_| ServiceError::terminal("device_registration_invalid"))?;
        if challenge.parent_flow_id.is_some()
            || challenge.device.descriptor != expected
            || challenge.device.registration.koru_identity_id != owner.account
            || challenge.device.registration.platform != self.platform
            || challenge.device.registration.app_version != self.app_version
            || challenge.device.registration.enrollment_protocol_version != current_enrollment
            || challenge.device.registration.peer_protocol_version != current_peer
        {
            return Err(ServiceError::terminal("device_registration_invalid"));
        }
        let proof = challenge.sign(&self.signing_key);
        request.device_ownership_proof = Some(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&proof)
                    .map_err(|_| ServiceError::terminal("device_registration_invalid"))?,
            ),
        );
        let registered = self
            .client
            .register_device(&request, &authorization)
            .map_err(map_client_error)?;
        if registered.device_id != self.descriptor.device_id
            || registered.revoked
            || registered.display_name != self.descriptor.display_name
            || registered.platform != self.platform
            || registered.app_version != self.app_version
            || registered.enrollment_protocol_version != current_enrollment
            || registered.peer_protocol_version != current_peer
        {
            return Err(ServiceError::terminal("device_registration_invalid"));
        }
        Ok(())
    }
    fn fetch_profiles(&mut self) -> Result<(), ServiceError> {
        self.profiles = self
            .client
            .profiles(&self.authorization()?)
            .map_err(map_client_error)?
            .profiles;
        Ok(())
    }

    fn refresh_profiles(&mut self) -> Result<Vec<AccountProfileRow>, ServiceError> {
        self.fetch_profiles()?;
        let held = self.held_identities();
        Ok(self
            .profiles
            .iter()
            .map(|profile| profile_row(profile, held.get(&profile.profile_id)))
            .collect())
    }

    /// Each Profile's identity as this device's own documents read it, keyed by
    /// Profile id.
    ///
    /// **This is the truth an account row is only a stand-in for.** See
    /// [`lifecycle::LocalProfileIdentity`]: nothing updates the account's copy
    /// after a Profile is created, so a row that preferred it would show a
    /// creation-time name forever. A Store this thread cannot reach answers
    /// nothing, which leaves every row on that first-run copy rather than
    /// failing a listing the person asked for.
    fn held_identities(&self) -> BTreeMap<ProfileId, lifecycle::LocalProfileIdentity> {
        self.local_profiles
            .identities()
            .unwrap_or_default()
            .into_iter()
            .map(|identity| (identity.profile_id, identity))
            .collect()
    }

    /// Every hosted Profile plus the independently derived places its content
    /// is available. Never filter here: authorization determines which rows a
    /// person may choose, while availability determines what action is honest.
    fn refresh_profile_choices(
        &mut self,
    ) -> Result<(String, Vec<AccountProfileChoice>), ServiceError> {
        // Not `refresh_profiles`: that would read the local identities once for
        // rows this discards, and again below.
        self.fetch_profiles()?;
        let account = self
            .client
            .account(&self.authorization()?)
            .map_err(map_client_error)?;
        let devices = self
            .client
            .devices(&self.authorization()?)
            .map_err(map_client_error)?
            .devices;
        let local_device = self.descriptor.device_id;
        let inventory = self.local_profiles.inventory()?;
        let held = self.held_identities();
        let choices = self
            .profiles
            .iter()
            .map(|profile| {
                let (available_on_this_device, available_on_another_device) =
                    lifecycle::profile_availability(
                        &inventory,
                        account.koru_identity_id,
                        &devices,
                        local_device,
                        profile.profile_id,
                    );
                AccountProfileChoice {
                    profile: profile_row(profile, held.get(&profile.profile_id)),
                    available_on_this_device,
                    available_on_another_device,
                }
            })
            .collect();
        Ok((account.display_name, choices))
    }
    fn inspect_signed_in_identity(&self) -> Result<(String, String), ServiceError> {
        let session = self
            .client
            .session(&self.authorization()?)
            .map_err(map_client_error)?;
        let account = self
            .client
            .account(&self.authorization()?)
            .map_err(map_client_error)?;
        let identity = account
            .auth_identities
            .into_iter()
            .find(|identity| identity.auth_identity_id == session.auth_identity_id)
            .ok_or_else(|| ServiceError::terminal("signed_in_identity_unavailable"))?;
        let provider = match identity.provider {
            koru_account_core::AuthProvider::Google => "google".to_owned(),
            koru_account_core::AuthProvider::Email => "email".to_owned(),
            koru_account_core::AuthProvider::Apple => "apple".to_owned(),
        };
        let verified_email = identity
            .verified_email_snapshot
            .ok_or_else(|| ServiceError::terminal("signed_in_identity_unavailable"))?;
        Ok((provider, verified_email))
    }

    fn delete_profile(
        &mut self,
        profile: ProfileId,
        expected_revision: koru_account_core::ProfileLifecycleRevision,
        cancel: &Cancel,
    ) -> Result<(koru_agent_wire::AccountProfileLifecycleRow, bool), ServiceError> {
        if cancel.cancelled() {
            return Err(ServiceError::terminal("cancelled"));
        }
        let now = self
            .clock
            .now_millis()
            .ok_or_else(|| ServiceError::terminal("clock_unavailable"))?;
        let account = self
            .durable_owner
            .ok_or_else(|| ServiceError::terminal("credential_owner_unavailable"))?
            .account;
        let pending = {
            let mut ownership = self
                .lifecycle_ownership
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut journal = self.journal();
            let mut existing = journal
                .get(profile)
                .map_err(|error| ServiceError::retryable(error.code))?;
            // A pending delete the service refused before applying anything
            // (a fresh sign-in was wanted) has nothing to replay. Left in
            // place, it outlived the in-memory owner proof across a restart
            // and refused every retry as owner-unproven; this also clears a
            // row written before refusals of that kind were dropped at once.
            if let Some(record) = &existing
                && record.stage == koru_agent_wire::ProfileCleanupStage::HostedPending
                && record
                    .failure_code
                    .as_deref()
                    .is_some_and(lifecycle::refusal_applied_nothing)
                && journal
                    .abandon_unapplied(profile, record.operation)
                    .map_err(|error| ServiceError::retryable(error.code))?
            {
                existing = None;
            }
            ownership.validate(profile, account, existing.as_ref())?;
            if existing.is_none() {
                let hosted_owner = self
                    .profiles
                    .iter()
                    .any(|known| known.profile_id == profile && known.koru_identity_id == account);
                let local_owner = self
                    .local_profiles
                    .inventory()?
                    .profiles
                    .iter()
                    .any(|item| {
                        item.profile_id.as_uuid() == profile.as_uuid()
                            && item.account_binding == format!("koru:{account}")
                    });
                if !hosted_owner && !local_owner {
                    return Err(ServiceError::terminal("profile_lifecycle_owner_unproven"));
                }
            }
            let pending = journal
                .begin_delete(profile, expected_revision, now)
                .map_err(|error| ServiceError::retryable(error.code))?;
            ownership.remember(&pending, account);
            pending
        };
        let response = match self.client.delete_profile(
            profile,
            &DeleteProfileRequestV1 {
                version: Version1,
                operation_id: pending.operation,
                expected_lifecycle_revision: pending.expected_revision,
            },
            &self.authorization()?,
        ) {
            Ok(response) => response,
            Err(error) => {
                let mapped = map_client_error(error);
                if lifecycle::refusal_applied_nothing(mapped.code) {
                    // Nothing happened on the service: forget the intent, so
                    // the delete after the fresh sign-in is a new one and
                    // proves its owner again, restart or not.
                    self.journal()
                        .abandon_unapplied(profile, pending.operation)
                        .map_err(|error| ServiceError::retryable(error.code))?;
                } else {
                    self.journal()
                        .record_failure(profile, pending.operation, mapped.code, now)
                        .map_err(|error| ServiceError::retryable(error.code))?;
                }
                return Err(mapped);
            }
        };
        // This write precedes the result. If it fails, the pending operation id
        // remains durable and the next request replays the exact hosted delete.
        let stored = self
            .journal()
            .record_hosted(profile, pending.operation, &response.lifecycle, now)
            .map_err(|error| ServiceError::retryable(error.code))?;
        self.profiles.retain(|known| known.profile_id != profile);
        Ok((stored.wire(), lifecycle::exact_retry(response.status)))
    }

    fn restore_profile(
        &mut self,
        profile: ProfileId,
        expected_revision: koru_account_core::ProfileLifecycleRevision,
        name_snapshot: String,
        locale_snapshot: Option<String>,
        cancel: &Cancel,
    ) -> Result<(koru_agent_wire::AccountProfileLifecycleRow, bool), ServiceError> {
        if cancel.cancelled() {
            return Err(ServiceError::terminal("cancelled"));
        }
        let now = self
            .clock
            .now_millis()
            .ok_or_else(|| ServiceError::terminal("clock_unavailable"))?;
        let operation = self
            .journal()
            .begin_restore(profile, expected_revision, now)
            .map_err(|error| ServiceError::retryable(error.code))?;
        let response = self
            .client
            .restore_profile(
                profile,
                &RestoreProfileRequestV1 {
                    version: Version1,
                    operation_id: operation,
                    expected_lifecycle_revision: expected_revision,
                    name_snapshot: Some(name_snapshot),
                    locale_snapshot,
                },
                &self.authorization()?,
            )
            .map_err(map_client_error)?;
        self.journal()
            .record_restored(&response.lifecycle, operation, now)
            .map_err(|error| ServiceError::retryable(error.code))?;
        let _ = self.refresh_profiles()?;
        Ok((
            lifecycle::wire_lifecycle(&response.lifecycle, None, None),
            lifecycle::exact_retry(response.status),
        ))
    }

    fn reconcile_profile_lifecycles(
        &mut self,
    ) -> Result<Vec<koru_agent_wire::AccountProfileLifecycleRow>, ServiceError> {
        let inventory = self.local_profiles.inventory()?;
        let account = self
            .client
            .account(&self.authorization()?)
            .map_err(map_client_error)?;
        let binding = format!("koru:{}", account.koru_identity_id);
        let profile_ids = inventory
            .profiles
            .iter()
            .filter(|item| item.account_binding == binding)
            .map(|item| ProfileId::from_uuid(item.profile_id.as_uuid()))
            .collect::<Vec<_>>();
        if profile_ids.len() > koru_account_core::MAX_PROFILE_LIFECYCLE_RESOLVE_IDS {
            return Err(ServiceError::terminal(
                "profile_lifecycle_inventory_too_large",
            ));
        }
        if profile_ids.is_empty() {
            return Ok(Vec::new());
        }
        let response = self
            .client
            .resolve_profile_lifecycles(
                &ResolveProfileLifecyclesRequestV1 {
                    version: Version1,
                    profile_ids,
                },
                &self.authorization()?,
            )
            .map_err(map_client_error)?;
        let now = self
            .clock
            .now_millis()
            .ok_or_else(|| ServiceError::terminal("clock_unavailable"))?;
        response
            .lifecycles
            .into_iter()
            .map(|item| {
                if item.koru_identity_id != account.koru_identity_id {
                    return Err(ServiceError::terminal(
                        "profile_lifecycle_identity_mismatch",
                    ));
                }
                if item.state == koru_account_core::ProfileLifecycleState::Deleted {
                    self.journal()
                        .observe_deleted(&item, now)
                        .map(|record| record.wire())
                        .map_err(|error| ServiceError::retryable(error.code))
                } else {
                    self.journal()
                        .observe_active(&item)
                        .map_err(|error| ServiceError::retryable(error.code))?;
                    Ok(lifecycle::wire_lifecycle(&item, None, None))
                }
            })
            .collect()
    }

    fn next_pending_delete(&self) -> Result<Option<AccountCommand>, ServiceError> {
        let Some(owner) = self.durable_owner else {
            return Ok(None);
        };
        let inventory = self.local_profiles.inventory()?;
        let binding = format!("koru:{}", owner.account);
        let ownership = self
            .lifecycle_ownership
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for record in self
            .journal()
            .pending()
            .map_err(|error| ServiceError::retryable(error.code))?
        {
            if record.stage != koru_agent_wire::ProfileCleanupStage::HostedPending
                || !inventory.profiles.iter().any(|item| {
                    item.profile_id.as_uuid() == record.profile.as_uuid()
                        && item.account_binding == binding
                })
            {
                continue;
            }
            ownership.validate(record.profile, owner.account, Some(&record))?;
            return Ok(Some(AccountCommand::DeleteProfile {
                profile_id: record.profile.to_string(),
                expected_lifecycle_revision: record.expected_revision.get(),
            }));
        }
        Ok(None)
    }
    /// Which page the browser is sent to — Google's or Apple's, named by the
    /// `start_command`. Everything else about a browser sign-in — the loopback
    /// listener, the callback parsing, the collision second leg, the exchange —
    /// is identical, which is why this is a parameter rather than a second
    /// copy of the function.
    fn start_browser(
        &mut self,
        generation: u64,
        start_command: AccountCommand,
        cancel: &Cancel,
        progress: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        let (locale, apple) = match &start_command {
            AccountCommand::StartGoogle { locale } => (locale.clone(), false),
            AccountCommand::StartApple { locale } => (locale.clone(), true),
            _ => return Err(ServiceError::terminal("invalid_request")),
        };
        validate_locale(&locale)?;
        if self.pending_creation.is_some() {
            return self.finish_creation(generation, None, cancel, progress);
        }
        if let Some(pending) = &self.pending_exchange {
            if pending.command == start_command {
                return self.finish_exchange(generation);
            }
            return Err(ServiceError::terminal("account_retry_pending"));
        }
        // The native account sheet is Google's; email never reaches here, because
        // V2 gave it a journey of its own that opens no browser at all. Apple
        // has no sheet, so even a phone takes the browser path for it.
        if self.delivery.supports_native_token() && !apple {
            return self.start_native_token(generation, locale, cancel, progress);
        }
        let mut listener = self.delivery.bind()?;
        let client_state = random_secret(self.entropy.as_ref());
        let verifier = random_secret(self.entropy.as_ref());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let start = {
            let request = GoogleNativeStartRequestV1 {
                version: Version1,
                enrollment_protocol_version: Some(koru_pairing::ENROLLMENT_VERSION),
                peer_protocol_version: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
                descriptor: self.descriptor.clone(),
                platform: self.platform,
                app_version: self.app_version.clone(),
                callback_port: if listener.app_link_return() {
                    0
                } else {
                    listener.callback_port()
                },
                client_state: OAuthState(client_state.clone()),
                pkce_challenge: challenge,
                locale: locale.clone(),
                app_link_return: listener.app_link_return(),
            };
            if apple {
                self.client.apple_native_start(&request)
            } else {
                self.client.google_native_start(&request)
            }
            .map_err(map_client_error)?
        };
        progress(ServiceProgress::State(
            self.state(generation, AccountStage::WaitingForBrowser),
        ));
        progress(ServiceProgress::Browser(checked_browser_url(
            start.authorization_url,
        )?));

        let first = listener.wait(&client_state, cancel)?;
        let (flow_id, grant) = match parse_callback(&first, true, self.clock.as_ref())? {
            ParsedCallback::Confirmation { authority, .. } => {
                self.pending_creation = Some(creation::PendingCreation::browser(
                    authority,
                    client_state,
                    verifier,
                    start_command,
                    start.flow_id,
                    listener,
                ));
                // Starting sign-in also authorizes creating an unseen account.
                // Keep the server-owned decision receipt for exact retry/cancel.
                return self.finish_creation(generation, Some(true), cancel, progress);
            }
            ParsedCallback::Completed(grant) => (start.flow_id, grant),
            ParsedCallback::Collision {
                flow_id,
                state,
                url,
            } => {
                progress(ServiceProgress::Browser(url));
                let second = listener.wait(&state, cancel)?;
                let ParsedCallback::Completed(grant) =
                    parse_callback(&second, false, self.clock.as_ref())?
                else {
                    return Err(ServiceError::terminal("authentication_failed"));
                };
                (flow_id, grant)
            }
            ParsedCallback::Rejected => {
                return Err(ServiceError::terminal("authentication_failed"));
            }
        };
        if cancel.cancelled() {
            return Err(ServiceError::terminal("cancelled"));
        }
        progress(ServiceProgress::State(
            self.state(generation, AccountStage::Exchanging),
        ));
        self.pending_exchange = Some(Pending {
            command: start_command,
            request: NativeExchangeRequestV1 {
                device_ownership_proof: None,
                version: Version1,
                flow_id,
                grant: OneUseGrant(grant),
                client_state: OAuthState(client_state),
                pkce_verifier: PkceVerifier(verifier),
            },
        });
        self.finish_exchange(generation)
    }
    /// Open a flow this device is bound to, before Koru knows which journey
    /// it is.
    ///
    /// `email_native_start` seals the signed descriptor and the PKCE challenge
    /// into the flow, and both endings redeem their grant against it: a
    /// sign-in that succeeds at once, and one that turns into a sign-up and
    /// picks up an address on the way. Opening a second flow for the address
    /// would leave the account created on a device Koru never registered.
    fn open_email_flow(
        &mut self,
        locale: &str,
    ) -> Result<(AuthFlowId, String, String), ServiceError> {
        let client_state = random_secret(self.entropy.as_ref());
        let verifier = random_secret(self.entropy.as_ref());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let opened = self
            .client
            .email_native_start(&EmailNativeStartRequestV1 {
                version: Version1,
                enrollment_protocol_version: Some(koru_pairing::ENROLLMENT_VERSION),
                peer_protocol_version: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
                descriptor: self.descriptor.clone(),
                platform: self.platform,
                app_version: self.app_version.clone(),
                client_state: OAuthState(client_state.clone()),
                pkce_challenge: challenge,
                locale: locale.to_owned(),
            })
            .map_err(map_client_error)?;
        Ok((opened.flow_id, client_state, verifier))
    }

    /// The welcome screen's one submit: an address and a password.
    ///
    /// **A wrong password and an address nobody has answer identically**, by
    /// mailing a code and parking at the code screen. That is what lets the
    /// the maintainers' one screen ask for both without becoming a way to find out who
    /// has an account; the disclosure waits for `verify`'s
    /// `already_registered`, which only somebody reading that mailbox can
    /// reach (D8). The web form takes the same two steps in
    /// `submitAddressAndPassword`; this is the copy the desktop and the phone
    /// share, and the reason there is not a third.
    fn continue_with_email(
        &mut self,
        generation: u64,
        locale: String,
        address: String,
        password: String,
        cancel: &Cancel,
        progress: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        validate_locale(&locale)?;
        if address.trim().is_empty() || address.len() > MAX_EMAIL_ADDRESS_BYTES {
            return Err(ServiceError::terminal("email_address_invalid"));
        }
        let (flow_id, client_state, verifier) = self.open_email_flow(&locale)?;
        let outcome = self
            .client
            .email_signin(&EmailSignInRequestV1 {
                version: Version1,
                address: address.clone(),
                password,
                client_state: OAuthState(client_state.clone()),
                flow_id: Some(flow_id),
            })
            .map_err(map_client_error)?;
        let pending = PendingEmail {
            flow_id,
            client_state: client_state.clone(),
            verifier,
            address: address.clone(),
            code: None,
        };
        match outcome {
            EmailSignInResponseV1::Completed { grant, .. } => {
                self.pending_email = Some(pending);
                self.redeem_email_grant(generation, grant, cancel, progress)
            }
            EmailSignInResponseV1::Rejected { .. } => {
                // The address attaches to the flow already holding this
                // device, so whatever account the journey ends at, it is this
                // device that was registered against it.
                self.client
                    .email_start(&EmailStartRequestV1 {
                        version: Version1,
                        address,
                        locale,
                        client_state: OAuthState(client_state),
                        flow_id: Some(flow_id),
                    })
                    .map_err(map_client_error)?;
                self.pending_email = Some(pending);
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::AwaitingEmailCode),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::SignedOut,
                    follow_up: None,
                })
            }
        }
    }

    /// Mail the code again, to the address the flow already holds.
    ///
    /// The sixty-second cooldown is the server's, and a refusal leaves the
    /// code screen exactly where it is — the first code may still arrive.
    fn resend_email_code(&mut self, generation: u64) -> Result<ServiceOutcome, ServiceError> {
        let pending = self
            .pending_email
            .as_ref()
            .ok_or_else(|| ServiceError::terminal("email_journey_absent"))?;
        let outcome = self
            .client
            .email_resend(&EmailResendRequestV1 {
                version: Version1,
                flow_id: pending.flow_id,
                address: pending.address.clone(),
            })
            .map_err(map_client_error)?;
        match outcome {
            EmailResendResponseV1::Sent { .. } => Ok(ServiceOutcome {
                state: self.state(generation, AccountStage::AwaitingEmailCode),
                credential: CredentialChange::Keep,
                result: AccountOperationResult::SignedOut,
                follow_up: None,
            }),
            EmailResendResponseV1::TooSoon { .. } => {
                Err(ServiceError::retryable("email_resend_too_soon"))
            }
        }
    }

    /// Six digits, and the one place they are checked from this client.
    ///
    /// A wrong code leaves the journey where it is: the person types again, and
    /// the five-guess bound lives on the server where a client cannot argue
    /// with it.
    fn submit_email_code(
        &mut self,
        generation: u64,
        code: String,
    ) -> Result<ServiceOutcome, ServiceError> {
        let pending = self
            .pending_email
            .as_ref()
            .ok_or_else(|| ServiceError::terminal("email_journey_absent"))?;
        let proved = code.clone();
        let verdict = self
            .client
            .email_verify(&EmailVerifyRequestV1 {
                version: Version1,
                flow_id: pending.flow_id,
                address: pending.address.clone(),
                code,
            })
            .map_err(map_client_error)?;
        match verdict {
            EmailVerifyResponseV1::PasswordRequired { .. } => {
                // **The journey keeps the code.** `complete` re-checks it
                // server-side, deliberately, so that a password can only be
                // set by somebody who still holds it -- and after V2 dropped
                // the link, this process is the only place it can be held
                // between the two screens.
                if let Some(pending) = self.pending_email.as_mut() {
                    pending.code = Some(proved);
                }
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::AwaitingEmailPassword),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::SignedOut,
                    follow_up: None,
                })
            }
            // Every refusal keeps the code screen up with a reason. Only
            // `AttemptsExhausted` ends the journey, because the challenge is
            // dead and the way forward is a new code.
            EmailVerifyResponseV1::CodeIncorrect { .. } => {
                Err(ServiceError::retryable("email_code_incorrect"))
            }
            EmailVerifyResponseV1::CodeExpired { .. } => {
                Err(ServiceError::retryable("email_code_expired"))
            }
            EmailVerifyResponseV1::AttemptsExhausted { .. } => {
                self.pending_email = None;
                Err(ServiceError::terminal("email_code_attempts_exhausted"))
            }
            EmailVerifyResponseV1::AlreadyRegistered { .. } => {
                self.pending_email = None;
                Err(ServiceError::terminal("email_already_registered"))
            }
            EmailVerifyResponseV1::AlreadyLinkedToGoogle { .. } => {
                self.pending_email = None;
                Err(ServiceError::terminal("email_already_linked_to_google"))
            }
        }
    }

    /// Set the password, and redeem what it earns.
    fn submit_email_password(
        &mut self,
        generation: u64,
        password: String,
        cancel: &Cancel,
        progress: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        let pending = self
            .pending_email
            .as_ref()
            .ok_or_else(|| ServiceError::terminal("email_journey_absent"))?;
        let code = pending
            .code
            .clone()
            .ok_or_else(|| ServiceError::terminal("email_code_unproved"))?;
        let completed = self
            .client
            .email_complete(&EmailCompleteRequestV1 {
                version: Version1,
                flow_id: pending.flow_id,
                address: pending.address.clone(),
                code,
                password,
                accepted_terms: true,
            })
            .map_err(map_client_error)?;
        match completed {
            EmailCompleteResponseV1::Completed { grant, .. } => {
                self.redeem_email_grant(generation, grant, cancel, progress)
            }
            EmailCompleteResponseV1::PasswordRefused { .. } => {
                Err(ServiceError::retryable("email_password_refused"))
            }
            EmailCompleteResponseV1::AddressTaken { .. } => {
                self.pending_email = None;
                Err(ServiceError::terminal("email_address_taken"))
            }
        }
    }

    /// The last step of both journeys: the grant becomes a session, and this
    /// device is registered against the account it belongs to.
    fn redeem_email_grant(
        &mut self,
        generation: u64,
        grant: OneUseGrant,
        cancel: &Cancel,
        progress: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        let pending = self
            .pending_email
            .take()
            .ok_or_else(|| ServiceError::terminal("email_journey_absent"))?;
        if cancel.cancelled() {
            return Err(ServiceError::terminal("cancelled"));
        }
        progress(ServiceProgress::State(
            self.state(generation, AccountStage::Exchanging),
        ));
        // Taken by clone rather than by move: `PendingEmail` zeroizes on drop,
        // and moving a field out of it would disarm that for the rest.
        let (flow_id, client_state, verifier) = (
            pending.flow_id,
            pending.client_state.clone(),
            pending.verifier.clone(),
        );
        drop(pending);
        self.pending_exchange = Some(Pending {
            command: AccountCommand::SubmitEmailPassword {
                password: String::new(),
            },
            request: NativeExchangeRequestV1 {
                device_ownership_proof: None,
                version: Version1,
                flow_id,
                grant,
                client_state: OAuthState(client_state),
                pkce_verifier: PkceVerifier(verifier),
            },
        });
        self.finish_exchange(generation)
    }

    fn link_rebind(
        &mut self,
        generation: u64,
        profile_id: ProfileId,
        erased: KoruIdentityId,
        cancel: &Cancel,
        progress: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ProfileSummaryV1, ServiceError> {
        if let Some(pending) = &self.pending_rebind {
            let command = AccountCommand::RebindProfile {
                profile_id: profile_id.to_string(),
                erased_identity_id: erased.to_string(),
            };
            if pending.command == command {
                return self.finish_rebind();
            }
            return Err(ServiceError::terminal("account_retry_pending"));
        }
        let mut listener = self.delivery.bind()?;
        let state = random_secret(self.entropy.as_ref());
        let start = self
            .client
            .link_google_start(
                &LinkGoogleStartRequestV1 {
                    version: Version1,
                    intent: LinkGoogleIntentV1::Rebind {
                        profile_id,
                        erased_koru_identity_id: erased,
                    },
                    callback: LinkCallbackDeliveryV1::Native {
                        callback_port: listener.callback_port(),
                        client_state: OAuthState(state.clone()),
                    },
                    locale: "en".into(),
                },
                &self.authorization()?,
            )
            .map_err(map_client_error)?;
        progress(ServiceProgress::State(
            self.state(generation, AccountStage::WaitingForBrowser),
        ));
        progress(ServiceProgress::Browser(checked_browser_url(
            start.authorization_url,
        )?));
        let callback = listener.wait(&state, cancel)?;
        let ParsedCallback::Completed(grant) =
            parse_callback(&callback, false, self.clock.as_ref())?
        else {
            return Err(ServiceError::terminal("authentication_failed"));
        };
        self.pending_rebind = Some(Pending {
            command: AccountCommand::RebindProfile {
                profile_id: profile_id.to_string(),
                erased_identity_id: erased.to_string(),
            },
            request: RebindProfileRequestV1 {
                version: Version1,
                profile_id,
                erased_koru_identity_id: erased,
                fresh_auth_grant: OneUseGrant(grant),
                explicit_rebind: true,
                name_snapshot: None,
                locale_snapshot: None,
            },
        });
        self.finish_rebind()
    }
}

impl AccountService for ProductionAccountService {
    fn cloud_profiles(
        &self,
        account: koru_account_core::KoruIdentityId,
    ) -> Vec<koru_agent_wire::AccountProfileCloud> {
        self.cloud_profile_choices(account)
    }
    fn fork(&self) -> Option<Box<dyn AccountService>> {
        Some(Box::new(Self {
            delivery: self.delivery.clone(),
            clock: self.clock.clone(),
            entropy: self.entropy.clone(),
            client: self.client.clone(),
            signing_key: self.signing_key.clone(),
            descriptor: self.descriptor.clone(),
            platform: self.platform,
            app_version: self.app_version.clone(),
            access: None,
            access_expires_at_millis: None,
            profiles: Vec::new(),
            pending_exchange: None,
            pending_native: None,
            pending_creation: None,
            pending_email: None,
            pending_logout: false,
            pending_link: None,
            pending_rebind: None,
            enrollment: self.enrollment.clone(),
            cloud: self.cloud.clone(),
            responder_session: self.responder_session.clone(),
            local_profiles: self.local_profiles.clone(),
            cloud_sync: self.cloud_sync.clone(),
            cloud_access: None,
            session_checked_at_millis: None,
            cloud_opened: std::collections::BTreeSet::new(),
            lifecycle_repository: self.lifecycle_repository.clone(),
            lifecycle_ownership: self.lifecycle_ownership.clone(),
            directory_namespace: self.directory_namespace.clone(),
            durable_owner: None,
            durable_record_version: None,
            unacknowledged_session_id: None,
            removed_credential_owner: None,
            durable_session_id: None,
            refresh_retry_key: None,
            device_registration_due: false,
            visible: None,
        }))
    }
    fn set_visibility(&mut self, visible: Arc<AtomicBool>) {
        self.visible = Some(visible);
    }
    fn enrollment_authority(&self) -> Option<crate::enrollment::EnrollmentAuthorization> {
        self.enrollment_authorization()
    }
    fn access_refresh_at_millis(&self) -> Option<i64> {
        if self.unacknowledged_session_id.is_some()
            || self.durable_session_id.is_none()
            || self.pending_logout
            || !self.cancellation_safe()
        {
            return None;
        }
        // The first startup attempt may be cancelled with the transient client
        // that caused the Agent to enter the foreground. A restored session
        // still needs a fresh access token so it can refresh stale device
        // registration and serve enrollment without another login.
        if self.access.is_none() {
            return self.clock.now_millis();
        }
        self.access_expires_at_millis?.checked_sub(60_000)
    }
    fn inspect_credential(
        &mut self,
        credential: &SessionCredential,
    ) -> Result<crate::identity::AccountOwner, ServiceError> {
        let session_id = credential
            .session_id
            .parse()
            .map_err(|_| ServiceError::terminal("credential_invalid"))?;
        let session = self
            .client
            .inspect_native_session(&koru_account_wire::NativeSessionInspectRequestV1 {
                version: Version1,
                session_id,
                refresh_token: koru_account_wire::RefreshToken(credential.refresh_token.clone()),
                device_id: self.descriptor.device_id,
            })
            .map_err(|error| match error {
                ClientError::Api {
                    status: 401,
                    ref code,
                    ..
                } if code.as_str() == koru_account_wire::ErrorCode::UNAUTHENTICATED => {
                    ServiceError::terminal("credential_rejected")
                }
                other => map_client_error(other),
            })?;
        if session.session_id != session_id || session.device_id != Some(self.descriptor.device_id)
        {
            return Err(ServiceError::terminal("credential_owner_mismatch"));
        }
        Ok(crate::identity::AccountOwner {
            account: session.koru_identity_id,
            auth_identity: session.auth_identity_id,
        })
    }
    fn execute(
        &mut self,
        generation: u64,
        command: AccountCommand,
        restored: Option<&SessionCredential>,
        cancel: &Cancel,
        progress: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        validate_command(&command)?;
        if matches!(
            &command,
            AccountCommand::SelectProfiles { .. } | AccountCommand::RetryEnrollment { .. }
        ) && cancel.cancelled()
        {
            return Err(ServiceError::terminal("cancelled"));
        }
        if matches!(&command, AccountCommand::Retry) {
            if self.pending_logout {
                return self.execute(
                    generation,
                    AccountCommand::Logout,
                    restored,
                    cancel,
                    progress,
                );
            }
            if self.pending_exchange.is_some() {
                return self.finish_exchange(generation);
            }
            if self.pending_creation.is_some() {
                return self.finish_creation(generation, None, cancel, progress);
            }
            if let Some(pending) = &self.pending_native {
                return self.start_native_token(
                    generation,
                    pending.locale.clone(),
                    cancel,
                    progress,
                );
            }
            if (self.pending_link.is_some() || self.pending_rebind.is_some())
                && !self.access_is_fresh()
            {
                let restored = restored.ok_or_else(ServiceError::revoked)?;
                progress(ServiceProgress::State(
                    self.state(generation, AccountStage::Refreshing),
                ));
                let credential = self.refresh(restored)?;
                return Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::SignedIn),
                    credential: CredentialChange::Replace(credential),
                    result: AccountOperationResult::SignedIn,
                    follow_up: Some(AccountCommand::Retry),
                });
            }
            if self.pending_link.is_some() {
                return self.finish_link(generation);
            }
            if self.pending_rebind.is_some() {
                let profile = self.finish_rebind()?;
                self.remember_profile(&profile);
                return Ok(profile_outcome(self, generation, profile));
            }
        }
        if matches!(
            &command,
            AccountCommand::InspectAccountSession
                | AccountCommand::InspectCloudSubscription { .. }
                | AccountCommand::SetProfileCloudSync { .. }
                | AccountCommand::InspectCloudSync { .. }
        ) && restored.is_none()
        {
            return Err(ServiceError::terminal("authentication_required"));
        }
        if matches!(
            &command,
            AccountCommand::InspectAccountSession
                | AccountCommand::InspectCloudSubscription { .. }
                | AccountCommand::SetProfileCloudSync { .. }
                | AccountCommand::InspectCloudSync { .. }
                | AccountCommand::ListProfiles
                | AccountCommand::ListProfileChoices
                | AccountCommand::SelectProfiles { .. }
                | AccountCommand::RetryEnrollment { .. }
                | AccountCommand::CreateProfile { .. }
                | AccountCommand::AttachProfile { .. }
                | AccountCommand::AddGoogle { .. }
                | AccountCommand::RebindProfile { .. }
                | AccountCommand::RemoveProfileAuthorization { .. }
                | AccountCommand::DeleteProfile { .. }
                | AccountCommand::RestoreProfile { .. }
                | AccountCommand::ReconcileProfileLifecycles
        ) && !self.access_is_fresh()
        {
            let restored = restored.ok_or_else(|| ServiceError::revoked())?;
            progress(ServiceProgress::State(
                self.state(generation, AccountStage::Refreshing),
            ));
            let credential = self.refresh(restored)?;
            return Ok(ServiceOutcome {
                state: self.state(generation, AccountStage::SignedIn),
                credential: CredentialChange::Replace(credential),
                result: AccountOperationResult::SignedIn,
                follow_up: Some(command),
            });
        }
        match command {
            AccountCommand::SetProfileCloudSync { .. }
            | AccountCommand::InspectCloudSync { .. } => {
                match self.cloud_sync(generation, command, cancel) {
                    Ok(outcome) => Ok(outcome),
                    Err(error) if error.code == "cancelled" => Err(error),
                    // A refusal of the switch is an answer to the switch, not
                    // a change in whether the person is signed in: the stage
                    // stays as it is and the shell reads the code.
                    Err(error) => Ok(ServiceOutcome {
                        state: self.state(generation, AccountStage::SignedIn),
                        credential: CredentialChange::Keep,
                        result: AccountOperationResult::Failed {
                            code: error.code.into(),
                        },
                        follow_up: None,
                    }),
                }
            }
            AccountCommand::InspectCloudSubscription { request_id } => {
                let subscription = self
                    .client
                    .cloud_subscription_status(&self.authorization()?, &|| cancel.cancelled())
                    .ok();
                // Remembered for a Profile waiting to start syncing: this is
                // the moment paid or complimentary access is first seen.
                if let (Some(subscription), Some(owner)) = (subscription, self.durable_owner) {
                    self.cloud_access = Some((owner.account, subscription.cloud_access));
                }
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::SignedIn),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::CloudSubscription {
                        request_id,
                        current: subscription.map(|status| status.current),
                        access: subscription.map(|status| status.cloud_access),
                    },
                    follow_up: None,
                })
            }
            AccountCommand::ConfirmAccountCreation => {
                self.finish_creation(generation, Some(true), cancel, progress)
            }
            AccountCommand::SwitchAccount { .. } | AccountCommand::AddAccount { .. } => {
                Err(ServiceError::terminal("account_coordinator_required"))
            }
            AccountCommand::CancelAccountCreation => {
                self.finish_creation(generation, Some(false), cancel, progress)
            }
            AccountCommand::Status | AccountCommand::Cancel { .. } => {
                let signed_in = restored.is_some();
                Ok(ServiceOutcome {
                    state: self.state(
                        generation,
                        if signed_in {
                            AccountStage::SignedIn
                        } else {
                            AccountStage::SignedOut
                        },
                    ),
                    credential: CredentialChange::Keep,
                    result: if signed_in {
                        AccountOperationResult::SignedIn
                    } else {
                        AccountOperationResult::SignedOut
                    },
                    follow_up: None,
                })
            }
            start @ (AccountCommand::StartGoogle { .. } | AccountCommand::StartApple { .. }) => {
                self.start_browser(generation, start, cancel, progress)
            }
            AccountCommand::ContinueWithEmail {
                locale,
                address,
                password,
            } => self.continue_with_email(generation, locale, address, password, cancel, progress),
            AccountCommand::ResendEmailCode => self.resend_email_code(generation),
            AccountCommand::SubmitEmailCode { code } => self.submit_email_code(generation, code),
            AccountCommand::SubmitEmailPassword { password } => {
                self.submit_email_password(generation, password, cancel, progress)
            }
            AccountCommand::RetryEnrollment { profile_id } => {
                if let Some(enrollment) = &self.enrollment {
                    let authority = self
                        .enrollment_authorization()
                        .ok_or_else(|| ServiceError::terminal("credential_owner_unavailable"))?;
                    if !enrollment.retry(
                        authority,
                        profile_id.as_deref().map(parse_profile).transpose()?,
                    ) {
                        return Err(ServiceError::retryable(
                            crate::enrollment::ENROLLMENT_UNAVAILABLE,
                        ));
                    }
                }
                let signed_in = restored.is_some();
                Ok(ServiceOutcome {
                    state: self.state(
                        generation,
                        if signed_in {
                            AccountStage::SignedIn
                        } else {
                            AccountStage::SignedOut
                        },
                    ),
                    credential: CredentialChange::Keep,
                    result: if signed_in {
                        AccountOperationResult::SignedIn
                    } else {
                        AccountOperationResult::SignedOut
                    },
                    follow_up: None,
                })
            }
            AccountCommand::CancelEnrollment => {
                if let Some(enrollment) = &self.enrollment {
                    let authority = self
                        .enrollment_authorization()
                        .ok_or_else(|| ServiceError::terminal("credential_owner_unavailable"))?;
                    enrollment
                        .cancel_durable_for(authority)
                        .map_err(ServiceError::terminal)?;
                }
                let signed_in = restored.is_some();
                Ok(ServiceOutcome {
                    state: self.state(
                        generation,
                        if signed_in {
                            AccountStage::SignedIn
                        } else {
                            AccountStage::SignedOut
                        },
                    ),
                    credential: CredentialChange::Keep,
                    result: if signed_in {
                        AccountOperationResult::SignedIn
                    } else {
                        AccountOperationResult::SignedOut
                    },
                    follow_up: None,
                })
            }
            AccountCommand::Retry => {
                let Some(restored) = restored else {
                    return Ok(ServiceOutcome {
                        state: signed_out(generation),
                        credential: CredentialChange::Keep,
                        result: AccountOperationResult::SignedOut,
                        follow_up: None,
                    });
                };
                progress(ServiceProgress::State(
                    self.state(generation, AccountStage::Refreshing),
                ));
                let credential = self.refresh(restored)?;
                let mut state = self.state(generation, AccountStage::SignedIn);
                let (follow_up, result) = match self.next_pending_delete() {
                    Ok(next) => (next, AccountOperationResult::SignedIn),
                    Err(error) => {
                        state.failure_code = Some(error.code.into());
                        (
                            None,
                            AccountOperationResult::Failed {
                                code: error.code.into(),
                            },
                        )
                    }
                };
                Ok(ServiceOutcome {
                    state,
                    credential: CredentialChange::Replace(credential),
                    result,
                    follow_up,
                })
            }
            AccountCommand::InspectAccountSession => {
                let (provider, verified_email) = self.inspect_signed_in_identity()?;
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::SignedIn),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::SignedInIdentity {
                        provider,
                        verified_email,
                    },
                    follow_up: None,
                })
            }
            AccountCommand::ListProfiles => {
                let profiles = self.refresh_profiles()?;
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::SignedIn),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::Profiles { profiles },
                    follow_up: None,
                })
            }
            AccountCommand::ListProfileChoices => {
                let (display_name, choices) = self.refresh_profile_choices()?;
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::SignedIn),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::ProfileChoices {
                        display_name,
                        choices,
                    },
                    follow_up: None,
                })
            }
            AccountCommand::CreateProfile {
                profile_id,
                owner_display_name,
                name,
                locale,
            } => {
                validate_name(&owner_display_name)?;
                validate_name(&name)?;
                if let Some(locale) = locale.as_deref() {
                    validate_locale(locale)?;
                }
                self.client
                    .update_account(
                        &UpdateAccountRequestV1 {
                            version: Version1,
                            display_name: owner_display_name,
                        },
                        &self.authorization()?,
                    )
                    .map_err(map_client_error)?;
                let profile = self
                    .client
                    .create_profile(
                        &CreateProfileRequestV1 {
                            version: Version1,
                            profile_id: parse_profile(&profile_id)?,
                            name_snapshot: name,
                            locale_snapshot: locale,
                        },
                        &self.authorization()?,
                    )
                    .map_err(map_client_error)?;
                self.remember_profile(&profile);
                // Created with the account, so its Cloud sync starts on
                // (ADR-000-createdwithaccount). `AttachProfile` below is a
                // Profile that already existed, which is still asked.
                self.cloud_sync_starts_on(profile.koru_identity_id, profile.profile_id);
                Ok(profile_outcome(self, generation, profile))
            }
            AccountCommand::AttachProfile {
                profile_id,
                name_snapshot,
                locale_snapshot,
            } => {
                let profile = self
                    .client
                    .attach_profile(
                        &AttachProfileRequestV1 {
                            version: Version1,
                            profile_id: parse_profile(&profile_id)?,
                            name_snapshot,
                            locale_snapshot,
                        },
                        &self.authorization()?,
                    )
                    .map_err(map_client_error)?;
                self.remember_profile(&profile);
                Ok(profile_outcome(self, generation, profile))
            }
            AccountCommand::AddGoogle { profile_id, locale } => {
                validate_locale(&locale)?;
                let profile_id = parse_profile(&profile_id)?;
                let pending_command = AccountCommand::AddGoogle {
                    profile_id: profile_id.to_string(),
                    locale: locale.clone(),
                };
                if let Some(pending) = &self.pending_link {
                    if pending.command == pending_command {
                        return self.finish_link(generation);
                    }
                    return Err(ServiceError::terminal("account_retry_pending"));
                }
                let mut listener = self.delivery.bind()?;
                let state = random_secret(self.entropy.as_ref());
                let start = self
                    .client
                    .link_google_start(
                        &LinkGoogleStartRequestV1 {
                            version: Version1,
                            intent: LinkGoogleIntentV1::AddGoogle { profile_id },
                            callback: LinkCallbackDeliveryV1::Native {
                                callback_port: listener.callback_port(),
                                client_state: OAuthState(state.clone()),
                            },
                            locale,
                        },
                        &self.authorization()?,
                    )
                    .map_err(map_client_error)?;
                progress(ServiceProgress::State(
                    self.state(generation, AccountStage::WaitingForBrowser),
                ));
                progress(ServiceProgress::Browser(checked_browser_url(
                    start.authorization_url,
                )?));
                let callback = listener.wait(&state, cancel)?;
                let ParsedCallback::Completed(grant) =
                    parse_callback(&callback, false, self.clock.as_ref())?
                else {
                    return Err(ServiceError::terminal("authentication_failed"));
                };
                self.pending_link = Some(Pending {
                    command: pending_command,
                    request: LinkGoogleContinueRequestV1 {
                        version: Version1,
                        flow_id: start.flow_id,
                        proof_grant: OneUseGrant(grant),
                    },
                });
                self.finish_link(generation)
            }
            AccountCommand::ConfirmAddGoogle {
                candidate_id,
                profile_id,
            } => {
                let profile_id = parse_profile(&profile_id)?;
                let confirmed = self
                    .client
                    .confirm_add_google(
                        &ConfirmAddGoogleRequestV1 {
                            version: Version1,
                            candidate_id: AddGoogleCandidateId(candidate_id),
                            profile_id,
                        },
                        &self.authorization()?,
                    )
                    .map_err(map_client_error)?;
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::SignedIn),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::GoogleAuthorizationAdded {
                        koru_identity_id: confirmed.koru_identity_id.to_string(),
                        profile_id: confirmed.profile_id.to_string(),
                        already_authorized: matches!(
                            confirmed.status,
                            ConfirmAddGoogleStatusV1::AlreadyAuthorized
                        ),
                    },
                    follow_up: None,
                })
            }
            AccountCommand::CancelAddGoogle {
                candidate_id,
                profile_id,
            } => {
                let profile_id = parse_profile(&profile_id)?;
                let cancelled = self
                    .client
                    .cancel_add_google(
                        &CancelAddGoogleRequestV1 {
                            version: Version1,
                            candidate_id: AddGoogleCandidateId(candidate_id),
                            profile_id,
                        },
                        &self.authorization()?,
                    )
                    .map_err(map_client_error)?;
                let status = match cancelled.status {
                    CancelAddGoogleStatusV1::Cancelled => AddGoogleCancellationStatus::Cancelled,
                    CancelAddGoogleStatusV1::AlreadyCancelled => {
                        AddGoogleCancellationStatus::AlreadyCancelled
                    }
                    CancelAddGoogleStatusV1::AlreadyConfirmed => {
                        AddGoogleCancellationStatus::AlreadyConfirmed
                    }
                };
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::SignedIn),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::AddGoogleCancelled {
                        profile_id: cancelled.profile_id.to_string(),
                        status,
                    },
                    follow_up: None,
                })
            }
            AccountCommand::RebindProfile {
                profile_id,
                erased_identity_id,
            } => {
                let profile = self.link_rebind(
                    generation,
                    parse_profile(&profile_id)?,
                    KoruIdentityId::from_str(&erased_identity_id)
                        .map_err(|_| ServiceError::terminal("invalid_request"))?,
                    cancel,
                    progress,
                )?;
                self.remember_profile(&profile);
                Ok(profile_outcome(self, generation, profile))
            }
            AccountCommand::RemoveProfileAuthorization {
                profile_id,
                auth_identity_id,
                confirm_profile_offline_only,
            } => {
                let profile = parse_profile(&profile_id)?;
                let auth_identity = AuthIdentityId::from_str(&auth_identity_id)
                    .map_err(|_| ServiceError::terminal("invalid_request"))?;
                let removed = self
                    .client
                    .remove_profile_authorization(
                        profile,
                        &RemoveProfileAuthorizationRequestV1 {
                            version: Version1,
                            auth_identity_id: auth_identity,
                            confirm_profile_offline_only,
                        },
                        &self.authorization()?,
                    )
                    .map_err(map_client_error)?;
                let _ = self.refresh_profiles()?;
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::SignedIn),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::ProfileAuthorizationRemoved {
                        profile_id,
                        profile_offline_only: removed.profile_offline_only,
                        already_removed: removed.status
                            == ProfileAuthorizationRemovalStatusV1::AlreadyRemoved,
                    },
                    follow_up: None,
                })
            }
            AccountCommand::DeleteProfile {
                profile_id,
                expected_lifecycle_revision,
            } => {
                let deleted = self.delete_profile(
                    parse_profile(&profile_id)?,
                    koru_account_core::ProfileLifecycleRevision::new(expected_lifecycle_revision),
                    cancel,
                );
                // Only a failed HTTP call was logged before, so a delete that
                // was refused here, or that succeeded, left no line at all.
                match &deleted {
                    Ok(_) => tracing::info!(
                        target: "koru_account",
                        "profile delete: the service committed it"
                    ),
                    Err(error) => tracing::info!(
                        target: "koru_account",
                        code = error.code,
                        retryable = error.retryable,
                        "profile delete: not committed"
                    ),
                }
                let (lifecycle, exact_retry) = deleted?;
                let mut state = self.state(generation, AccountStage::SignedIn);
                let follow_up = match self.next_pending_delete() {
                    Ok(next) => next,
                    Err(error) => {
                        state.failure_code = Some(error.code.into());
                        None
                    }
                };
                Ok(ServiceOutcome {
                    state,
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::ProfileDeleted {
                        lifecycle,
                        exact_retry,
                    },
                    follow_up,
                })
            }
            AccountCommand::RestoreProfile {
                profile_id,
                expected_lifecycle_revision,
                name_snapshot,
                locale_snapshot,
            } => {
                let (lifecycle, exact_retry) = self.restore_profile(
                    parse_profile(&profile_id)?,
                    koru_account_core::ProfileLifecycleRevision::new(expected_lifecycle_revision),
                    name_snapshot,
                    locale_snapshot,
                    cancel,
                )?;
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::SignedIn),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::ProfileRestored {
                        lifecycle,
                        exact_retry,
                    },
                    follow_up: None,
                })
            }
            AccountCommand::ReconcileProfileLifecycles => {
                let profiles = self.reconcile_profile_lifecycles()?;
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::SignedIn),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::ProfileLifecycleReconciliation { profiles },
                    follow_up: None,
                })
            }
            AccountCommand::InspectProfileLifecycle { profile_id } => {
                // A settled row is the journal's memory of a finished
                // deletion, kept so a re-delivered tombstone starts nothing.
                // To a caller asking what is in flight it is what it was
                // before the row was kept: nothing.
                let lifecycle = self
                    .journal()
                    .get(parse_profile(&profile_id)?)
                    .map_err(|error| ServiceError::retryable(error.code))?
                    .filter(|record| {
                        record.stage != koru_agent_wire::ProfileCleanupStage::NavigationSettled
                    })
                    .map(|record| record.wire());
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::SignedIn),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::ProfileLifecycle { lifecycle },
                    follow_up: None,
                })
            }
            AccountCommand::AcknowledgeProfileCleanup { profile_id, stage } => {
                let profile = parse_profile(&profile_id)?;
                let now = self
                    .clock
                    .now_millis()
                    .ok_or_else(|| ServiceError::terminal("clock_unavailable"))?;
                let lifecycle = self
                    .journal()
                    .acknowledge(profile, stage, now)
                    .map_err(|error| ServiceError::retryable(error.code))?
                    .map(|record| record.wire());
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::SignedIn),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::ProfileLifecycle { lifecycle },
                    follow_up: None,
                })
            }
            AccountCommand::SelectProfiles { profile_ids } => {
                let profiles = profile_ids
                    .iter()
                    .map(|id| parse_profile(id))
                    .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
                let authorization = self
                    .enrollment_authorization()
                    .ok_or_else(|| ServiceError::terminal("credential_owner_unavailable"))?;
                let enrollment = self.enrollment.clone().ok_or_else(|| {
                    ServiceError::terminal(crate::enrollment::ENROLLMENT_UNAVAILABLE)
                })?;
                self.provision_selected_profiles(&profiles, &authorization, cancel)?;
                let chosen = profiles.clone();
                let snapshot = enrollment
                    .select_profiles(profiles, authorization)
                    .map_err(ServiceError::terminal)?;
                // Chosen onto this device from the account, so their Cloud
                // sync starts on (ADR-000-createdwithaccount). Until the
                // Store is opened to Cloud for them, nothing of theirs can
                // arrive here at all: every pull is `mode_refuses_transport`.
                if let Some(owner) = self.durable_owner {
                    for profile in chosen {
                        self.cloud_sync_starts_on(owner.account, profile);
                    }
                }
                Ok(ServiceOutcome {
                    state: self.state(generation, AccountStage::SignedIn),
                    credential: CredentialChange::Keep,
                    result: AccountOperationResult::ProfilesSelected {
                        profiles: snapshot
                            .profiles
                            .iter()
                            .map(crate::enrollment::ProfileReport::wire)
                            .collect(),
                    },
                    follow_up: None,
                })
            }
            AccountCommand::Logout => {
                self.pending_logout = true;
                if self
                    .pending_creation
                    .as_ref()
                    .is_some_and(|creation| creation.issued_session_id.is_none())
                {
                    // Resolve the chosen server decision before dropping its
                    // authority. Existing logout then revokes any issued session.
                    let outcome =
                        self.finish_creation(generation, Some(false), cancel, progress)?;
                    if outcome.state.failure_code.as_deref() == Some("account_confirmation_expired")
                    {
                        // An expired replay cannot prove revocation or absence
                        // of a remote session. The worker owns local erasure.
                        return Ok(outcome);
                    }
                }
                if let (Some(session), Some(owner), Some(id)) = (
                    &self.responder_session,
                    self.durable_owner,
                    &self.durable_session_id,
                ) {
                    session.disable_account_session_and_wait(owner.account, id);
                }
                let remove_enrollment = self.directory_namespace.is_none()
                    || self.removed_credential_owner.is_some_and(|removed| {
                        self.durable_owner
                            .is_some_and(|active| active.account == removed.account)
                    });
                let enrollment_clear = self
                    .enrollment
                    .as_ref()
                    .filter(|_| remove_enrollment)
                    .map(|enrollment| match self.enrollment_authorization() {
                        Some(authority) => enrollment.cancel_durable_for(authority),
                        None if self.visible.is_none() => enrollment.cancel_durable(),
                        None => Ok(()),
                    })
                    .transpose()
                    .map_err(|_| {
                        ServiceError::logout_incomplete(crate::enrollment::PERSISTENCE_FAILED)
                    });
                let remote = self.remote_logout(restored);
                if remote.as_ref().is_err_and(|error| error.retryable) && !self.cancellation_safe()
                {
                    return remote.map(|_| unreachable!("checked error"));
                }
                self.access = None;
                self.access_expires_at_millis = None;
                self.profiles.clear();
                self.pending_exchange = None;
                self.pending_native = None;
                self.pending_logout = false;
                self.pending_creation = None;
                self.pending_link = None;
                self.pending_rebind = None;
                self.durable_owner = None;
                self.unacknowledged_session_id = None;
                enrollment_clear?;
                remote?;
                Ok(ServiceOutcome {
                    state: signed_out(generation),
                    credential: CredentialChange::Remove,
                    result: AccountOperationResult::SignedOut,
                    follow_up: None,
                })
            }
        }
    }

    fn credential_acknowledged(&mut self) {
        if let Some(session_id) = self.unacknowledged_session_id.take() {
            self.durable_session_id = Some(session_id);
        }
        self.unacknowledged_session_id = None;
        self.pending_creation = None;
        self.pending_exchange = None;
        self.pending_native = None;
        if self.visible.is_none()
            && let (Some(session), Some(owner), Some(session_id)) = (
                &self.responder_session,
                self.durable_owner,
                &self.durable_session_id,
            )
        {
            session.enable_account(owner.account, session_id.clone());
        }
    }

    fn credential_owner_acknowledged(&mut self, record: Option<crate::identity::DirectoryRecord>) {
        self.durable_owner = record.as_ref().map(|record| record.owner);
        self.durable_record_version = record.as_ref().map(|record| record.version);
        self.credential_acknowledged();
        if self.visible.is_none()
            && let Some(authorization) = self.enrollment_authorization()
        {
            if let Some(enrollment) = &self.enrollment {
                enrollment.update_authorization(authorization.clone());
            }
            if let Some(cloud) = &self.cloud {
                cloud.update_authorization(authorization);
            }
        }
        // The credential a rotation produced is durable, so the hosted calls
        // it owes may run: a failure here costs a stale registration that the
        // next refresh owes again, where before it cost the rotation itself.
        // A restore arrives here with no access token and owes nothing.
        if self.device_registration_due
            && let Some(access) = self.access.clone()
        {
            self.device_registration_due = false;
            if let Err(error) = self.refresh_device_registration(&access) {
                self.device_registration_due = true;
                tracing::info!(
                    target: "koru_account",
                    code = error.code,
                    retryable = error.retryable,
                    "device registration refresh deferred after a rotation"
                );
            }
        }
    }

    fn arm_refresh_retry_key(&mut self, key: Option<[u8; 32]>) {
        self.refresh_retry_key = key;
    }

    fn credential_restored(
        &mut self,
        credential: &SessionCredential,
        record: Option<crate::identity::DirectoryRecord>,
    ) {
        self.durable_session_id = Some(credential.session_id.clone());
        // The vault holds only the session and its refresh token; the access
        // token lives in memory. A fresh process therefore restores with
        // none, and the next line it logs should be a refresh.
        let remaining = self
            .access_expires_at_millis
            .zip(self.clock.now_millis())
            .map(|(expires, now)| expires.saturating_sub(now));
        tracing::info!(
            target: "koru_account",
            access_held = self.access.is_some(),
            access_remaining_millis = ?remaining,
            access_expired = remaining.is_none_or(|millis| millis <= 0),
            "credential restored"
        );
        self.credential_owner_acknowledged(record);
    }

    fn credential_removed(&mut self, owner: Option<crate::identity::AccountOwner>) {
        self.removed_credential_owner = owner;
    }

    fn cancelled(&mut self, _generation: u64) {
        self.pending_creation = None;
        self.pending_exchange = None;
        self.pending_native = None;
        self.pending_link = None;
        self.pending_rebind = None;
    }

    fn cancellation_safe(&self) -> bool {
        self.pending_creation.is_none()
            && self.pending_exchange.is_none()
            && self
                .pending_native
                .as_ref()
                .is_none_or(|pending| !pending.may_have_committed())
            && self.pending_link.is_none()
            && self.pending_rebind.is_none()
    }
}

fn profile_outcome(
    service: &ProductionAccountService,
    generation: u64,
    profile: ProfileSummaryV1,
) -> ServiceOutcome {
    ServiceOutcome {
        state: service.state(generation, AccountStage::SignedIn),
        credential: CredentialChange::Keep,
        result: AccountOperationResult::Profile {
            profile: profile_row(&profile, service.held_identities().get(&profile.profile_id)),
        },
        follow_up: None,
    }
}

/// One hosted Profile as a surface should read it: **its own identity document
/// where this device holds one, and the account's copy only where it does not.**
///
/// The account's `name_snapshot` is written at creation and at restore and never
/// again — there is no rename route, by decision — so a row that took it in
/// preference would show the name a Profile had on the day it was made for the
/// rest of its life. It survives here as the one thing a brand new device can
/// show: it has the Profile ids from its account session and no documents yet,
/// and a list of nameless Profiles on first run is worse than a stale one.
fn profile_row(
    profile: &ProfileSummaryV1,
    held: Option<&lifecycle::LocalProfileIdentity>,
) -> AccountProfileRow {
    AccountProfileRow {
        koru_identity_id: profile.koru_identity_id.to_string(),
        profile_id: profile.profile_id.to_string(),
        name: held
            .map(|identity| identity.name.clone())
            .or_else(|| profile.name_snapshot.clone()),
        locale: held
            .and_then(|identity| identity.locale.clone())
            .or_else(|| profile.locale_snapshot.clone()),
        auth_identities: profile
            .auth_identities
            .iter()
            .map(|identity| AccountAuthIdentityRow {
                auth_identity_id: identity.auth_identity_id.to_string(),
                provider: match identity.provider {
                    koru_account_core::AuthProvider::Google => "google".into(),
                    koru_account_core::AuthProvider::Email => "email".into(),
                    koru_account_core::AuthProvider::Apple => "apple".into(),
                },
                verified_email: identity.verified_email_snapshot.clone(),
            })
            .collect(),
    }
}
fn parse_profile(value: &str) -> Result<ProfileId, ServiceError> {
    ProfileId::parse(value).ok_or_else(|| ServiceError::terminal("invalid_profile_id"))
}
fn canonical_enrollment_profile(value: &str) -> Result<ProfileId, ServiceError> {
    let id =
        uuid::Uuid::parse_str(value).map_err(|_| ServiceError::terminal("invalid_profile_id"))?;
    if id.get_version_num() != 4
        || id.get_variant() != uuid::Variant::RFC4122
        || id.to_string() != value
    {
        return Err(ServiceError::terminal("invalid_profile_id"));
    }
    parse_profile(value)
}
fn validate_locale(value: &str) -> Result<(), ServiceError> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(ServiceError::terminal("invalid_locale"));
    }
    Ok(())
}
fn validate_name(value: &str) -> Result<(), ServiceError> {
    if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(ServiceError::terminal("invalid_profile_name"));
    }
    Ok(())
}
fn validate_candidate_id(value: &str) -> Result<(), ServiceError> {
    if value.is_empty() || value.len() > 2_048 || !value.is_ascii() {
        return Err(ServiceError::terminal("invalid_request"));
    }
    Ok(())
}
fn validate_command(command: &AccountCommand) -> Result<(), ServiceError> {
    match command {
        AccountCommand::SwitchAccount { account_id } => KoruIdentityId::from_str(account_id)
            .map(|_| ())
            .map_err(|_| ServiceError::terminal("invalid_request")),
        AccountCommand::AddAccount { locale } => validate_locale(locale),
        AccountCommand::ConfirmAccountCreation | AccountCommand::CancelAccountCreation => Ok(()),
        AccountCommand::SubmitEmailCode { code } => (!code.trim().is_empty() && code.len() <= 64)
            .then_some(())
            .ok_or(ServiceError::terminal("email_code_invalid")),
        AccountCommand::SubmitEmailPassword { password } => (!password.is_empty()
            && password.len() <= MAX_PASSWORD_BYTES)
            .then_some(())
            .ok_or(ServiceError::terminal("email_password_invalid")),
        AccountCommand::ContinueWithEmail {
            locale, address, ..
        } => {
            validate_locale(locale)?;
            (!address.trim().is_empty() && address.len() <= MAX_EMAIL_ADDRESS_BYTES)
                .then_some(())
                .ok_or(ServiceError::terminal("email_address_invalid"))
        }
        AccountCommand::StartGoogle { locale } | AccountCommand::StartApple { locale } => {
            validate_locale(locale)
        }
        AccountCommand::CreateProfile {
            profile_id,
            owner_display_name,
            name,
            locale,
        } => {
            parse_profile(profile_id)?;
            validate_name(owner_display_name)?;
            validate_name(name)?;
            if let Some(locale) = locale {
                validate_locale(locale)?;
            }
            Ok(())
        }
        AccountCommand::AttachProfile {
            profile_id,
            name_snapshot,
            locale_snapshot,
        } => {
            parse_profile(profile_id)?;
            if let Some(name) = name_snapshot {
                validate_name(name)?;
            }
            if let Some(locale) = locale_snapshot {
                validate_locale(locale)?;
            }
            Ok(())
        }
        AccountCommand::SetProfileCloudSync { profile_id, .. } => {
            parse_profile(profile_id).map(|_| ())
        }
        AccountCommand::SelectProfiles { profile_ids } => {
            let mut previous: Option<&str> = None;
            for profile in profile_ids {
                canonical_enrollment_profile(profile)?;
                if previous.is_some_and(|previous| previous >= profile.as_str()) {
                    return Err(ServiceError::terminal("invalid_request"));
                }
                previous = Some(profile);
            }
            Ok(())
        }
        AccountCommand::RetryEnrollment { profile_id } => profile_id
            .as_deref()
            .map(canonical_enrollment_profile)
            .transpose()
            .map(|_| ()),
        AccountCommand::AddGoogle { profile_id, locale } => {
            parse_profile(profile_id)?;
            validate_locale(locale)
        }
        AccountCommand::ConfirmAddGoogle {
            candidate_id,
            profile_id,
        }
        | AccountCommand::CancelAddGoogle {
            candidate_id,
            profile_id,
        } => {
            parse_profile(profile_id)?;
            validate_candidate_id(candidate_id)
        }
        AccountCommand::RemoveProfileAuthorization {
            profile_id,
            auth_identity_id,
            ..
        } => {
            parse_profile(profile_id)?;
            AuthIdentityId::from_str(auth_identity_id)
                .map(|_| ())
                .map_err(|_| ServiceError::terminal("invalid_request"))
        }
        AccountCommand::DeleteProfile { profile_id, .. }
        | AccountCommand::RestoreProfile { profile_id, .. }
        | AccountCommand::InspectProfileLifecycle { profile_id }
        | AccountCommand::AcknowledgeProfileCleanup { profile_id, .. } => {
            parse_profile(profile_id).map(|_| ())
        }
        AccountCommand::RebindProfile {
            profile_id,
            erased_identity_id,
        } => {
            parse_profile(profile_id)?;
            KoruIdentityId::from_str(erased_identity_id)
                .map(|_| ())
                .map_err(|_| ServiceError::terminal("invalid_request"))
        }
        AccountCommand::Status
        | AccountCommand::Cancel { .. }
        // Carries nothing, so there is nothing to validate; the flow it
        // resends for is the worker's own.
        | AccountCommand::ResendEmailCode
        | AccountCommand::Retry
        | AccountCommand::CancelEnrollment
        | AccountCommand::InspectAccountSession
        | AccountCommand::InspectCloudSubscription { .. }
        | AccountCommand::InspectCloudSync { .. }
        | AccountCommand::ListProfiles
        | AccountCommand::ListProfileChoices
        | AccountCommand::ReconcileProfileLifecycles
        | AccountCommand::Logout => Ok(()),
    }
}
fn random_secret(entropy: &dyn Entropy) -> String {
    let mut bytes = [0_u8; 32];
    entropy.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
/// Where this client is willing to send a browser.
///
/// **Not a formality.** The URL arrives from the server, and without this an
/// API that had been tampered with could point somebody's browser anywhere and
/// have the app open it for them.
///
/// Google's and Apple's sign-in each live at exactly one host, and since V2
/// moved the email journey into the app there is no other destination: nothing
/// else opens a browser. An Apple sign-in can still hand back Google's page for
/// the collision proof, so both hosts are admitted whichever provider started.
fn checked_browser_url(value: String) -> Result<BrowserUrl, ServiceError> {
    if value.len() > 8192 {
        return Err(ServiceError::terminal("invalid_browser_url"));
    }
    let parsed =
        url::Url::parse(&value).map_err(|_| ServiceError::terminal("invalid_browser_url"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ServiceError::terminal("invalid_browser_url"));
    }
    let admitted = parsed.scheme() == "https"
        && matches!(
            parsed.host_str(),
            Some("accounts.google.com" | "appleid.apple.com")
        )
        && parsed.port().is_none();
    if !admitted {
        return Err(ServiceError::terminal("invalid_browser_url"));
    }
    Ok(BrowserUrl(value))
}

enum ParsedCallback {
    Confirmation {
        authority: String,
    },
    Completed(String),
    Collision {
        flow_id: AuthFlowId,
        state: String,
        url: BrowserUrl,
    },
    Rejected,
}
fn parse_callback(
    callback: &Callback,
    allow_collision: bool,
    clock: &dyn Clock,
) -> Result<ParsedCallback, ServiceError> {
    match exact(callback, "outcome")? {
        "confirmation_required" if allow_collision => {
            exact_fields(
                callback,
                &["state", "outcome", "confirmation", "expires_at_millis"],
            )?;
            validate_expiry(exact(callback, "expires_at_millis")?, clock)?;
            let authority = exact(callback, "confirmation")?;
            if authority.len() > 1024 || !authority.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(ServiceError::terminal("callback_invalid"));
            }
            Ok(ParsedCallback::Confirmation {
                authority: authority.to_owned(),
            })
        }
        "completed" => {
            exact_fields(
                callback,
                &[
                    "state",
                    "outcome",
                    "grant",
                    "return_target",
                    "expires_at_millis",
                ],
            )?;
            if exact(callback, "return_target")? != "account" {
                return Err(ServiceError::terminal("callback_invalid"));
            }
            validate_expiry(exact(callback, "expires_at_millis")?, clock)?;
            let grant = exact(callback, "grant")?;
            if grant.len() > 4096 || !grant.bytes().all(|byte| byte.is_ascii_graphic()) {
                return Err(ServiceError::terminal("callback_invalid"));
            }
            Ok(ParsedCallback::Completed(grant.to_owned()))
        }
        "collision_required" if allow_collision => {
            exact_fields(
                callback,
                &[
                    "state",
                    "outcome",
                    "collision_flow_id",
                    "reauthentication_flow_id",
                    "authorization_url",
                    "expires_at_millis",
                    "reauthentication_state",
                ],
            )?;
            validate_expiry(exact(callback, "expires_at_millis")?, clock)?;
            let collision = AuthFlowId::from_str(exact(callback, "collision_flow_id")?)
                .map_err(|_| ServiceError::terminal("callback_invalid"))?;
            let flow_id = AuthFlowId::from_str(exact(callback, "reauthentication_flow_id")?)
                .map_err(|_| ServiceError::terminal("callback_invalid"))?;
            if collision == flow_id {
                return Err(ServiceError::terminal("callback_invalid"));
            }
            let state = exact(callback, "reauthentication_state")?;
            if state.len() > 4096 || !state.bytes().all(|byte| byte.is_ascii_graphic()) {
                return Err(ServiceError::terminal("callback_invalid"));
            }
            Ok(ParsedCallback::Collision {
                flow_id,
                state: state.to_owned(),
                url: checked_browser_url(exact(callback, "authorization_url")?.to_owned())?,
            })
        }
        "provider_rejected" | "account_mismatch" | "cancelled" => {
            exact_fields(callback, &["state", "outcome"])?;
            Ok(ParsedCallback::Rejected)
        }
        _ => Err(ServiceError::terminal("callback_invalid")),
    }
}
fn exact_fields(callback: &Callback, allowed: &[&str]) -> Result<(), ServiceError> {
    if callback.field_names().count() != allowed.len()
        || callback
            .field_names()
            .any(|field| !allowed.contains(&field))
        || allowed
            .iter()
            .any(|field| callback.values(field).count() != 1)
    {
        return Err(ServiceError::terminal("callback_invalid"));
    }
    Ok(())
}
fn validate_expiry(value: &str, clock: &dyn Clock) -> Result<(), ServiceError> {
    let expiry = value
        .parse::<i64>()
        .map_err(|_| ServiceError::terminal("callback_invalid"))?;
    let now = clock
        .now_millis()
        .ok_or_else(|| ServiceError::terminal("clock_invalid"))?;
    if expiry <= now {
        return Err(ServiceError::terminal("callback_expired"));
    }
    Ok(())
}
fn exact<'a>(callback: &'a Callback, field: &'a str) -> Result<&'a str, ServiceError> {
    let mut values = callback.values(field);
    let value = values
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ServiceError::terminal("callback_invalid"))?;
    if values.next().is_some() {
        return Err(ServiceError::terminal("callback_invalid"));
    }
    Ok(value)
}
fn map_client_error(error: ClientError) -> ServiceError {
    match error {
        ClientError::Api { status: 428, code }
            if code.as_str()
                == koru_account_wire::ErrorCode::PROFILE_OFFLINE_ONLY_CONFIRMATION_REQUIRED =>
        {
            ServiceError::terminal(
                koru_account_wire::ErrorCode::PROFILE_OFFLINE_ONLY_CONFIRMATION_REQUIRED,
            )
        }
        ClientError::Api { status: 428, code }
            if code.as_str()
                == koru_account_wire::ErrorCode::FRESH_GOOGLE_AUTHENTICATION_REQUIRED =>
        {
            ServiceError::terminal(
                koru_account_wire::ErrorCode::FRESH_GOOGLE_AUTHENTICATION_REQUIRED,
            )
        }
        ClientError::Api { status: 428, code }
            if code.as_str() == koru_account_wire::ErrorCode::FRESH_AUTHENTICATION_REQUIRED =>
        {
            ServiceError::terminal(koru_account_wire::ErrorCode::FRESH_AUTHENTICATION_REQUIRED)
        }
        // A refusal the service did not word keeps the reading its status
        // earns, now that the status survives. Only the transient statuses
        // are listed: a codeless 401 or 403 deliberately does **not** join
        // the `authentication_required` arm below, because a bare 401 from a
        // proxy, a CDN or a captive portal is not the service saying the
        // credential is dead, and reading it as one would sign a person out
        // on somebody else's say-so.
        ClientError::Transport(_)
        | ClientError::InvalidResponse
        | ClientError::RateLimited { .. }
        | ClientError::Api {
            status: 408 | 429 | 500..=599,
            ..
        }
        | ClientError::ApiWithoutCode {
            status: 408 | 429 | 500..=599,
        } => ServiceError::retryable("account_unavailable"),
        ClientError::Api {
            status: 401 | 403, ..
        } => ServiceError::terminal("authentication_required"),
        _ => ServiceError::terminal("account_request_failed"),
    }
}

fn refresh_was_revoked(error: &ClientError) -> bool {
    match error {
        ClientError::Api {
            status: 401 | 403, ..
        } => true,
        ClientError::Api { code, .. } => matches!(
            code.as_str(),
            koru_account_wire::ErrorCode::FLOW_CONSUMED
                | koru_account_wire::ErrorCode::UNAUTHENTICATED
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
