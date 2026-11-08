// SPDX-License-Identifier: MPL-2.0

//! Portable Cloud sync coordination shared by the desktop Agent and the
//! Android host.
//!
//! The coordinator owns exactly one thing: turning a live account
//! authorization into a bounded set of in-memory Cloud authority contexts and
//! handing each admitted dataset to the host that owns the dial, the bridge
//! and the reconciliation pass. It performs no networking, opens no Store and
//! persists nothing: every platform effect is a [`Host`] method and every
//! hosted read is an [`Api`] method, so the whole state machine runs in tests
//! with in-memory fakes, no disk, no network and no telemetry.
//!
//! It reuses [`crate::enrollment::EnrollmentAuthorization`] deliberately.
//! There is one verified account authorization on a device; a second
//! authorization type would be a second thing to keep in step with the
//! directory and the vault, and the account service pushes this one to both
//! drivers at the same moments.
//!
//! Nothing here survives a restart. Permits, connections, sequence numbers
//! and status all start empty and are rebuilt from current authority
//! (TECHNICAL §8): there is no persisted `synced` boolean, no receipt cache
//! and no timer that scans documents.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use koru_account_core::{KoruIdentityId, ProfileId, WorkspaceId};
use koru_account_wire::cloud_sync::{SyncCredential, SyncTokenResponseV1};
use koru_account_wire::{
    CloudSyncRouteV1, CloudWorkspaceCandidateV1, CloudWorkspaceCursorV1,
    CloudWorkspaceDescriptorV1, CloudWorkspacePageV1, JwksResponseV1,
};
use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1};
use koru_store_core::profile_units::ProfileUnitRef;
use uuid::Uuid;

use crate::enrollment::EnrollmentAuthorization;

mod control;
mod coordinator;
mod principal;
mod production;
pub use control::Control;
pub use principal::{Accepted, build_principal, control_permit, credential_claims, static_code};
pub use production::{HostedApi, capability_verifier};

/// The wake channel holds exactly one pending signal, as enrollment's does:
/// a burst of local changes is one pass, never a queue of passes.
const WAKE_CAPACITY: usize = 1;
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// TECHNICAL §8: the shared trailing debounce. A local commit or a remote
/// hint schedules the next pass 500 ms after the last such event.
pub const CHANGE_DEBOUNCE: Duration = Duration::from_millis(500);

/// Active admission threads, not the number of standing subscriptions.
pub const COHORT_WORKSPACES: usize = 16;
/// Lightweight subscriptions have a separate bound from active sync work.
pub const MAX_LIVE_PERMITS: usize = 4096;
pub const MAX_CARRIED_CANDIDATES: usize = MAX_LIVE_PERMITS / 2;

pub const CLOUD_OFFLINE: &str = "cloud_offline";
pub const CLOUD_UNAVAILABLE: &str = "cloud_unavailable";
pub const CLOUD_RATE_LIMITED: &str = "cloud_rate_limited";
pub const SELECTION_DENIED: &str = "cloud_selection_denied";
pub const DESCRIPTOR_NOT_READY: &str = "cloud_descriptor_not_ready";
pub const DATASET_MISMATCH: &str = "cloud_dataset_mismatch";
pub const WORKER_UNAVAILABLE: &str = "cloud_worker_unavailable";
pub const BOOTSTRAP_FAILED: &str = "cloud_bootstrap_failed";
/// Renew through admission; never extend a worker credential locally.
pub const CREDENTIAL_EXPIRED: &str = "credential_expired";
pub const SESSION_RENEW_BEFORE_SECONDS: i64 = 90;
pub const ACCESS_REVOKED: &str = "cloud_access_revoked";
pub const SESSION_EXPIRED: &str = "session_revoked";

/// Object-safe adapter boundary, mirroring [`crate::enrollment::Driver`], so
/// the account service pushes the same authorization to both without knowing
/// which effect port is behind either.
pub trait Driver: Send + Sync {
    fn update_authorization(&self, authority: EnrollmentAuthorization) -> bool;
    fn activate_owned(&self, authority: Option<EnrollmentAuthorization>, owner_scope: u64) -> bool;
    fn refresh_authorization(
        &self,
        previous: EnrollmentAuthorization,
        next: EnrollmentAuthorization,
    ) -> bool;
    fn set_foreground(&self, foreground: bool);
    /// A durable local commit, a connection change or a host resume. The pass
    /// is rescheduled behind the 500 ms trailing debounce, never run inline.
    fn wake(&self) -> bool;
    /// Close admission: fence every live permit and return to idle. It does
    /// not assert completion of disk work the host already accepted.
    fn cancel(&self) -> bool;
    fn snapshot(&self) -> Snapshot;
}

/// Set by the host; observed by the coordinator between every step of a pass.
/// A hosted call already in flight finishes resolving normally, exactly as an
/// in-flight account operation does — cancellation is a boundary, not a kill.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn request(&self) {
        self.0.store(true, Ordering::Release);
    }
    fn clear(&self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Every refusal the coordinator can act on. `NotReady` is the §3.1 503:
/// the hosted mapping does not exist yet, which is a reason to skip the
/// Workspace this pass, never a reason to invent an identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// A worker credential needs replacement, not an account sign-in.
    CredentialExpired,
    Offline,
    Unavailable,
    NotReady,
    /// The issued route says the dataset's worker is down. Reported and
    /// retried; it is never a reason to dial something else.
    WorkerUnavailable,
    Denied,
    SessionExpired,
    AccessRevoked,
    DatasetMismatch,
    Bootstrap,
    RateLimited(Duration),
}

impl Failure {
    pub fn code(&self) -> &'static str {
        match self {
            Self::CredentialExpired => CREDENTIAL_EXPIRED,
            Self::Offline => CLOUD_OFFLINE,
            Self::Unavailable => CLOUD_UNAVAILABLE,
            Self::NotReady => DESCRIPTOR_NOT_READY,
            Self::WorkerUnavailable => WORKER_UNAVAILABLE,
            Self::Denied => SELECTION_DENIED,
            Self::SessionExpired => SESSION_EXPIRED,
            Self::AccessRevoked => ACCESS_REVOKED,
            Self::DatasetMismatch => DATASET_MISMATCH,
            Self::Bootstrap => BOOTSTRAP_FAILED,
            Self::RateLimited(_) => CLOUD_RATE_LIMITED,
        }
    }
    /// Revocation and expiry are answered by a new authorization, not by a
    /// retry of the same request.
    fn automatic(&self) -> bool {
        !matches!(self, Self::SessionExpired | Self::AccessRevoked)
    }
}

/// HOST-CONTROL §3 `CloudStatus.stage`, exactly. `Settled` is the end of a
/// reconciliation pass; it is never durable Cloud coverage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    #[default]
    AwaitingAuthority,
    Connecting,
    Reconciling,
    WaitingForConsent,
    Offline,
    Unavailable,
    CheckingCoverage,
    Settled,
}

impl Stage {
    /// The exact control-protocol spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingAuthority => "awaiting_authority",
            Self::Connecting => "connecting",
            Self::Reconciling => "reconciling",
            Self::WaitingForConsent => "waiting_for_consent",
            Self::Offline => "offline",
            Self::Unavailable => "unavailable",
            Self::CheckingCoverage => "checking_coverage",
            Self::Settled => "settled",
        }
    }

    /// How loudly a dataset's stage speaks for the whole account. The least
    /// finished stage wins, so one offline dataset is never hidden behind
    /// another that settled — and `Settled` is the quietest of all, because
    /// it is the one stage that claims nothing.
    fn severity(self) -> u8 {
        match self {
            Self::Settled => 0,
            Self::CheckingCoverage => 1,
            Self::Reconciling => 2,
            Self::Connecting => 3,
            Self::WaitingForConsent => 4,
            Self::Unavailable => 5,
            Self::Offline => 6,
            Self::AwaitingAuthority => 7,
        }
    }
}

/// HOST-CONTROL §3's `CloudPermitV1`, with exactly the listed fields. It is
/// an admission limit for one dataset under one authority context, not proof
/// that any local content exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudPermitV1 {
    pub version: u32,
    pub permit_id: Uuid,
    pub dataset: CloudReplicaPrincipalV1,
    pub account_id: KoruIdentityId,
    pub profile_id: ProfileId,
    pub authority_workspace_id: CloudId,
    pub authority_workspace_incarnation_id: CloudId,
    pub account_generation: u64,
    pub expires_at_seconds: i64,
    /// Empty for Workspace datasets. For a Profile dataset, at most Identity,
    /// Private and the Personal unit of the exact authorizing Workspace; the
    /// host narrows further by the grant it can actually prove.
    pub allowed_profile_units: Vec<ProfileUnitRef>,
    /// Required for Workspace datasets, null for Profile datasets.
    pub workspace_descriptor: Option<CloudWorkspaceDescriptorV1>,
}

/// What the originating device already holds for a Workspace (HOST-CONTROL
/// §3.1 `read_cloud_bootstrap`). On the desktop the adapter asks the gateway
/// over the existing authenticated control channel, which reads the durable
/// manifest identity through Store; on Android the adapter reads the Store
/// directly through `StoreWorkerClient`. No valid local source returns
/// `None` for a Workspace this device holds: absence means a fresh device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalBootstrap {
    pub profile_id: ProfileId,
    pub workspace_id: WorkspaceId,
    pub manifest_document_id: Uuid,
}

/// What one dataset session reported before it gave the worker back. It is
/// in-memory status only: `Settled` is not coverage, and nothing here is
/// written down or carried across a restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionReport {
    pub stage: Stage,
    /// The remote said there is more to exchange. Reschedules the next pass
    /// behind the trailing debounce.
    pub remote_hint: bool,
    pub bytes_held: u64,
    pub queued_jobs: u32,
    /// At most two, per HOST-CONTROL §3.
    pub pending_consent_ids: Vec<Uuid>,
    pub error_code: Option<&'static str>,
}

impl Default for SessionReport {
    fn default() -> Self {
        Self {
            stage: Stage::Settled,
            remote_hint: false,
            bytes_held: 0,
            queued_jobs: 0,
            pending_consent_ids: Vec::new(),
            error_code: None,
        }
    }
}

/// The hosted account reads a Cloud pass makes. Every one is bounded, uses
/// the existing native bearer and returns `no-store` metadata; none of them
/// carries content, names or worker secrets.
pub trait Api: Send + 'static {
    /// TECHNICAL §6. `after` is an untrusted cursor position, never authority.
    fn discover(
        &mut self,
        authority: &EnrollmentAuthorization,
        after: Option<CloudWorkspaceCursorV1>,
    ) -> Result<CloudWorkspacePageV1, Failure>;

    /// TECHNICAL §7.1. Idempotent: repeating a selected pair answers
    /// `selected`. A refusal is [`Failure::Denied`] for this pass.
    fn select(
        &mut self,
        authority: &EnrollmentAuthorization,
        profile: ProfileId,
        workspace: WorkspaceId,
    ) -> Result<(), Failure>;

    /// HOST-CONTROL §3.1. [`Failure::NotReady`] is the 503
    /// `server_unavailable`: authorized, but no retained mapping yet.
    fn describe(
        &mut self,
        authority: &EnrollmentAuthorization,
        workspace: WorkspaceId,
        profile: ProfileId,
    ) -> Result<CloudWorkspaceDescriptorV1, Failure>;

    /// One short-lived credential and the two dataset routes for one pair.
    /// The response is evidence to correlate, never authority by itself.
    fn issue_token(
        &mut self,
        authority: &EnrollmentAuthorization,
        workspace: WorkspaceId,
        profile: ProfileId,
    ) -> Result<SyncTokenResponseV1, Failure>;

    /// The verification material the credential is checked against. The
    /// coordinator refreshes it once per account generation, before the first
    /// credential of that generation, so the dial never verifies a token
    /// against keys read under a replaced authority.
    fn jwks(&mut self) -> Result<JwksResponseV1, Failure>;
}

/// Every platform effect. The desktop adapter reaches the Store through the
/// gateway's typed client over the existing control channel; the Android
/// adapter reaches the same Store ports in process through `StoreWorkerClient`.
/// Neither is implemented here: this module never opens a Store file, dials a
/// route or frames a control message.
pub trait Host: Send + Sync + 'static {
    /// Does this device already hold the Workspace with a manifest identity?
    /// This is the originating-device branch of HOST-CONTROL §3.1
    /// (`read_cloud_bootstrap`): the account, Profile and the issued
    /// Workspace dataset are the verified context the desktop adapter sends
    /// over the control channel, and the Android adapter checks in process.
    /// `dataset` is always the Workspace arm; a Profile dataset is refused
    /// by every host.
    fn local_workspace(
        &self,
        account: KoruIdentityId,
        profile: ProfileId,
        dataset: CloudReplicaPrincipalV1,
    ) -> Result<Option<LocalBootstrap>, Failure>;

    /// Bootstrap step 1: `Store::provision_account_profile(id, account
    /// binding, name, locale, now)`. Returns the stored Profile unchanged if
    /// it exists, and never overwrites a different account binding.
    fn ensure_profile_presence(
        &self,
        profile: ProfileId,
        account: KoruIdentityId,
    ) -> Result<(), Failure>;

    /// Bootstrap step 3: `Store::provision_replica(profile, workspace,
    /// manifest_document_id, now)` with the Profile read back from the Store.
    /// No placeholder Profile is ever constructed for this call.
    fn ensure_workspace_presence(
        &self,
        descriptor: &CloudWorkspaceDescriptorV1,
    ) -> Result<(), Failure>;

    /// HOST-CONTROL §3 `replace_cloud_context`. The sequence is strictly
    /// increasing within one coordinator session; a restart invalidates it.
    fn replace_context(
        &self,
        sequence: u64,
        account: KoruIdentityId,
        account_generation: u64,
        permits: &[CloudPermitV1],
    ) -> Result<(), Failure>;

    /// HOST-CONTROL §3 `fence_cloud` with an explicit list. An empty list is
    /// a no-op and is never sent.
    fn fence(&self, permit_ids: &[Uuid]) -> Result<(), Failure>;

    /// The account's reach into the blob **control** plane
    /// (`cloud-blob-object-v1` §3), pushed on every pass.
    ///
    /// The account lives here and the Store lives in the host, and a blob
    /// transfer needs both: the five control routes take an account bearer
    /// token, and the referenced-blob walk, the staging file, the object
    /// install and the announcement are all the Store's. Splitting the
    /// transfer engine across the two would put half a state machine on each
    /// side of a boundary; handing over the one fact this side holds is the
    /// smaller seam.
    ///
    /// **Pushed every pass rather than on change**, so that a host stops
    /// making requests when this side goes quiet: the value carries how long
    /// it may be used *without* being confirmed again, not how long the token
    /// itself is valid.
    ///
    /// **No `sequence`**, unlike [`replace_context`](Self::replace_context).
    /// The coordinator's counter exists for the context's monotonic check;
    /// the ordering a gateway actually enforces is the adapter's own, on its
    /// one sequence space per control session, and a second number here would
    /// be a number two sides could disagree about for nothing.
    ///
    /// The default does nothing, because a host with no blob engine — Android
    /// until B5 — has nowhere to put it, and a required method would make
    /// every implementor write that sentence out.
    fn replace_blob_access(
        &self,
        account: KoruIdentityId,
        account_generation: u64,
        access: &koru_account_wire::AccessToken,
    ) -> Result<(), Failure> {
        let _ = (account, account_generation, access);
        Ok(())
    }

    /// Hand one admitted dataset to the host, which owns the dial, the
    /// handshake, the bridge and the reconciliation pass. `keys` is the
    /// verification material read under this account generation; the host
    /// checks the credential against `keys.capability_keys` before it dials
    /// and never against keys it cached under another authority.
    /// `access` observes accepted same-session JWT refreshes during the pass.
    /// Hosts maintain blob grants, stop on lost authority, and return
    /// `CredentialExpired` before the admitted worker credential expires.
    fn open_dataset(
        &self,
        permit: &CloudPermitV1,
        credential: &SyncCredential,
        route: &CloudSyncRouteV1,
        dataset: CloudReplicaPrincipalV1,
        keys: &JwksResponseV1,
        access: &SessionAccess,
    ) -> Result<SessionReport, Failure>;
}

mod credentials;
pub use credentials::SessionAccess;

/// One live permit's current in-memory state. This is HOST-CONTROL §3's
/// `CloudStatus` record: no credential string, name, title or content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusRecord {
    pub permit_id: Uuid,
    pub profile_id: ProfileId,
    pub account_id: KoruIdentityId,
    pub account_generation: u64,
    pub dataset: CloudReplicaPrincipalV1,
    pub stage: Stage,
    pub error_code: Option<&'static str>,
    pub pending_consent_ids: Vec<Uuid>,
    pub bytes_held: u64,
    pub queued_jobs: u32,
}

/// A pair the pass could not admit. Diagnostics for the host surface, held in
/// memory for this pass only; it is not part of `CloudStatus` and never input
/// to the coverage projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deferred {
    pub profile_id: ProfileId,
    pub workspace_id: WorkspaceId,
    pub code: &'static str,
}

/// One coherent cached observation. A host-side diagnostic value, not an
/// Agent protocol message: only [`Snapshot::records`] maps to `CloudStatus`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub version: u32,
    pub revision: u64,
    pub owner_scope: Option<u64>,
    pub account_id: Option<KoruIdentityId>,
    pub account_generation: u64,
    /// The overall stage. `AwaitingAuthority` whenever no authorization is held.
    pub stage: Stage,
    /// At most [`MAX_LIVE_PERMITS`], sorted by permit id.
    pub records: Vec<StatusRecord>,
    pub deferred: Vec<Deferred>,
    pub failure_code: Option<&'static str>,
    pub retry_at_millis: Option<i64>,
    /// Candidates enumerated but not in the current cohort.
    pub carried: usize,
    /// The last `replace_cloud_context` sequence this session sent.
    pub sequence: u64,
    pub discovery: Option<DiscoveryReceipt>,
}

mod discovery;
pub use discovery::{DiscoveryCounts, DiscoveryReceipt};

impl Snapshot {
    fn idle() -> Self {
        Self {
            version: 1,
            ..Self::default()
        }
    }
}

fn cloud_id(value: Uuid) -> CloudId {
    CloudId::from_bytes(*value.as_bytes())
}

/// Same account and same session. A new session id, or a different account,
/// is a replacement: it bumps the account generation and fences every permit.
fn same_authority(previous: &EnrollmentAuthorization, next: &EnrollmentAuthorization) -> bool {
    previous.owner == next.owner && previous.session_id == next.session_id
}

fn checked_retry_at(now_millis: i64, delay: Duration) -> Option<i64> {
    i64::try_from(delay.as_millis())
        .ok()
        .and_then(|delay| now_millis.checked_add(delay))
}

fn backoff(failures: u32) -> Duration {
    MIN_BACKOFF
        .checked_mul(1_u32 << failures.min(6))
        .unwrap_or(MAX_BACKOFF)
        .min(MAX_BACKOFF)
}

/// Commands the worker applies before it dispatches a pass.
enum Command {
    Activate(Option<EnrollmentAuthorization>, Option<u64>),
    Refresh(EnrollmentAuthorization, EnrollmentAuthorization),
    Wake,
    Cancel,
}

#[derive(Default)]
struct Desired {
    commands: VecDeque<Command>,
    stop: bool,
}

/// The visible cache the host reads while the worker is inside a slow effect.
type Visible = Arc<Mutex<Snapshot>>;

fn signal(wake: &mpsc::SyncSender<()>) -> bool {
    matches!(
        wake.try_send(()),
        Ok(()) | Err(mpsc::TrySendError::Full(()))
    )
}

#[cfg(test)]
mod tests;
