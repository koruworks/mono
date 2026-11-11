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
//! and no timer that scans documents. The one durable thing a pass writes
//! is written by the Store through the host — the account-scope binding and
//! the root-unit join receipts of root-catalog Plan 3.2 — and it is the
//! Store's, not this module's.
//!
//! **Admission is per scope since root-catalog Plan 3.2** (TECHNICAL.final
//! §7.3): a Workspace is admitted on its own id, membership, incarnation and
//! the device's own policy; the Profile it is placed under is a projection
//! the permit carries as a hint and never a prerequisite. The v1 shape — one
//! Workspace permit paired with one Profile permit — is kept only as the
//! fallback for a service that does not answer the v2 catalog routes yet.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use koru_account_core::root_catalog::ContentRefusal;
use koru_account_core::{CatalogScopeId, KoruIdentityId, ProfileId, WorkspaceId};
use koru_account_wire::cloud_sync::{SyncCredential, SyncTokenResponseV1};
use koru_account_wire::{
    CloudCatalogBindingRefV2, CloudCatalogBindingV2, CloudDiscoveryCursorV2,
    CloudDiscoveryReceiptV2, CloudRootCatalogV2, CloudRootJoinReceiptV2, CloudRootUnitUpdatesV2,
    CloudSyncRouteV1, CloudSyncScopeV2, CloudUnitRefV2, CloudWorkspaceCandidateV1,
    CloudWorkspaceCursorV1, CloudWorkspaceDescriptorV1, CloudWorkspaceDescriptorV2,
    CloudWorkspacePageV1, JwksResponseV1, SyncTokenResponseV2,
};
use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1};
use koru_store_core::service::JoinUnit;
pub use koru_sync_control::cloud::{
    CloudAccountBindingRef, CloudPermitScopeV2, CloudPermitV2, CloudRootExport, CloudRootJoin,
    CloudRootUnitState, CloudWorkspaceDescriptorV2 as ControlDescriptorV2,
};
use uuid::Uuid;

use crate::enrollment::EnrollmentAuthorization;

mod control;
mod coordinator;
mod principal;
mod production;
pub use control::Control;
pub use principal::{Accepted, build_principal, credential_claims, static_code};
pub use production::{HostedApi, capability_verifier};

/// The wake channel holds exactly one pending signal, as enrollment's does:
/// a burst of local changes is one pass, never a queue of passes.
const WAKE_CAPACITY: usize = 1;
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// TECHNICAL §8: the shared trailing debounce. A local commit or a remote
/// hint schedules the next pass 500 ms after the last such event.
pub const CHANGE_DEBOUNCE: Duration = Duration::from_millis(500);
/// How long a held Profile the service refused to select is left alone
/// before its Workspaces are asked for again. Long enough that a sign-in
/// that is not linked to it costs a handful of refused requests an hour
/// instead of one per Workspace per pass; short enough that linking it
/// again is picked up without restarting anything.
const REFUSED_PROFILE_RETRY: Duration = Duration::from_secs(300);

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
/// The service answered a v2 catalog route with 404: it does not speak the
/// v2 contract yet, and the pass runs the v1 compatibility path.
pub const ROUTE_NOT_FOUND: &str = "cloud_route_not_found";
/// This device's catalog is bound to another account (TECHNICAL.final
/// §4.1): terminal for this account, never a merge. The wire code of
/// `koru_account_core::root_catalog::BindingRefusal::AccountConflict`.
pub const CATALOG_BINDING_CONFLICT: &str = "catalog_binding_conflict";
/// The device declares this account's scope under another id than the
/// service holds; a binding is never renamed. Terminal.
pub const CATALOG_BINDING_MISMATCH: &str = "catalog_binding_mismatch";
/// A Workspace whose root placement this device cannot list it under yet
/// (Plan 2.1's `placement_pending`); retried after the next root join.
pub const PLACEMENT_PENDING: &str = "placement_pending";

// ----- The service's own coded refusals (QUESTIONS.md Q12) ---------------
//
// Four answers the service words itself were read as a generic outage and
// retried for ever. They are kept here **as the service spells them**: a
// diagnostic then names what was actually answered, and a support
// conversation can search for the same token on both sides.

/// This build cannot read that unit's format — a newer Koru wrote it. Q12
/// (a): the unit is dropped for this session and the person is told to
/// update; retrying a format this build does not have is a machine
/// repeating itself.
pub const ROOT_UNIT_FORMAT_TOO_NEW: &str = "root_unit_format_too_new";
/// The service will not serve or accept that one unit, and no retry can
/// change it (Q12 (b)). Diagnostics only: there is nothing a person can do,
/// and the unit's id is what makes it answerable later.
pub const ROOT_UNIT_INVALID: &str = "root_unit_invalid";
pub const ROOT_EXPORT_REFUSED: &str = "root_export_refused";
pub const ROOT_UNIT_ADDRESS_INVALID: &str = "root_unit_address_invalid";
/// A 403 for one unit: its placement, or the Profile unit itself, names a
/// Profile the account does not hold (root-catalog-cloud-v2 §admission).
/// It used to read as a denied principal, which failed the **whole** pass
/// every 30 s and called the Profile *unavailable* (2026-11-10). Per unit:
/// skipped this generation, logged by id, and asked again when the
/// generation is replaced — sign-in, account change, or Koru leaving the
/// foreground — by which time the Profile may be the account's.
pub const ROOT_PLACEMENT_FOREIGN: &str = "root_placement_foreign";
/// A personal unit whose Workspace unit the service has not admitted.
/// Per unit for the same reason; the Workspace unit goes first in the next
/// generation's pass.
pub const ROOT_WORKSPACE_UNIT_REQUIRED: &str = "root_workspace_unit_required";
/// 428: the route wants the account's catalog binding in place first. Q12
/// (c): the binding step runs again and the call is retried **once**.
pub const CATALOG_BINDING_REQUIRED: &str = "catalog_binding_required";
/// Service-side damage only an operator can repair (Q12 (d)). Retried with
/// backoff, and never worded as an outage.
pub const ROOT_PROJECTION_CORRUPT: &str = "root_projection_corrupt";
pub const CATALOG_BINDING_CORRUPT: &str = "catalog_binding_corrupt";

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
    /// A v2 route the service does not serve (404).
    RouteNotFound,
    /// This device's catalog is bound to another account. Terminal: the
    /// local catalog is left exactly as it was.
    CatalogBindingConflict,
    /// The binding this device holds for the account names another scope
    /// than the service's. Terminal.
    CatalogBindingMismatch,
    /// §7.3's content admission refused the Workspace; the code is the
    /// refusal's own (`mode_refuses_transport` when this device's policy
    /// closes the Cloud path for it).
    Content(ContentRefusal),
    /// No root placement this device can list the Workspace under yet.
    PlacementPending,
    /// Q12 (a): the unit is in a format a newer Koru wrote. Terminal for
    /// that unit, never for the pass — and the person is told to update,
    /// because that is the only thing that can change the answer.
    UnitFormatTooNew,
    /// Q12 (b): the service refused that one unit for good. Terminal for the
    /// unit and silent to the person; the id goes to sync diagnostics.
    UnitRefused(UnitRefusal),
    /// Q12 (c): a 428 naming the account binding as a precondition. The
    /// binding step runs again and the call is retried once; a second one
    /// takes the ordinary unavailable path.
    CatalogBindingRequired,
    /// Q12 (d): service-side damage to this account. Retried with backoff,
    /// and worded as a problem with the account rather than an outage.
    AccountDamaged(AccountDamage),
}

/// Which of the three terminal per-unit refusals the service answered. One
/// enum rather than a `&str`, so only the service's three codes can be in a
/// diagnostic, and each keeps its own word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitRefusal {
    UnitInvalid,
    ExportRefused,
    AddressInvalid,
    PlacementForeign,
    WorkspaceUnitRequired,
}

impl UnitRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::UnitInvalid => ROOT_UNIT_INVALID,
            Self::ExportRefused => ROOT_EXPORT_REFUSED,
            Self::AddressInvalid => ROOT_UNIT_ADDRESS_INVALID,
            Self::PlacementForeign => ROOT_PLACEMENT_FOREIGN,
            Self::WorkspaceUnitRequired => ROOT_WORKSPACE_UNIT_REQUIRED,
        }
    }
}

/// What the service says is damaged. Both read the same way to a person —
/// Koru Cloud has a problem with this account — and keep their own code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountDamage {
    RootProjection,
    CatalogBinding,
}

impl AccountDamage {
    pub fn code(self) -> &'static str {
        match self {
            Self::RootProjection => ROOT_PROJECTION_CORRUPT,
            Self::CatalogBinding => CATALOG_BINDING_CORRUPT,
        }
    }
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
            Self::RouteNotFound => ROUTE_NOT_FOUND,
            Self::CatalogBindingConflict => CATALOG_BINDING_CONFLICT,
            Self::CatalogBindingMismatch => CATALOG_BINDING_MISMATCH,
            Self::Content(refusal) => refusal.code(),
            Self::PlacementPending => PLACEMENT_PENDING,
            Self::UnitFormatTooNew => ROOT_UNIT_FORMAT_TOO_NEW,
            Self::UnitRefused(refusal) => refusal.code(),
            Self::CatalogBindingRequired => CATALOG_BINDING_REQUIRED,
            Self::AccountDamaged(damage) => damage.code(),
        }
    }
    /// Revocation, expiry and a binding refusal are answered by a new
    /// authorization or a person, not by a retry of the same request. The
    /// two terminal per-unit refusals are here for completeness only: they
    /// are answered where the unit is asked for and never become a pass's
    /// failure, because one unit must not stop the pass (Q12 (a), (b)).
    /// Service-side damage **is** retried: only an operator can repair it,
    /// and a client that stopped asking would never notice the repair.
    fn automatic(&self) -> bool {
        !matches!(
            self,
            Self::SessionExpired
                | Self::AccessRevoked
                | Self::CatalogBindingConflict
                | Self::CatalogBindingMismatch
                | Self::UnitFormatTooNew
                | Self::UnitRefused(_)
        )
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

/// Which Cloud contract the service answered this account generation with.
/// Probed once per generation on the binding route: a 404 is a service that
/// does not speak v2 yet, and the pass runs the v1 pair path unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudContract {
    /// The pre-root-catalog pair path (`discover`, `select`, `describe`,
    /// `issue_token`); permits are read as v2 at the host boundary.
    V1,
    /// Root-catalog Plan 3.1's contract: binding, root-unit join, per-scope
    /// tokens, discovery by current placement.
    V2,
}

/// What the originating device already holds for a Workspace (HOST-CONTROL
/// §3.1 `read_cloud_bootstrap`). On the desktop the adapter asks the gateway
/// over the existing authenticated control channel, which reads the durable
/// manifest identity through Store; on Android the adapter reads the Store
/// directly through `StoreWorkerClient`. No valid local source returns
/// `None` for a Workspace this device holds: absence means a fresh device.
/// No Profile is part of it (TECHNICAL.final §7.3): the Workspace's
/// placement is the root unit's business.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalBootstrap {
    pub workspace_id: WorkspaceId,
    pub manifest_document_id: Uuid,
    /// Whether this device's own transport policy for the Workspace admits
    /// the Cloud path — the Profile ceiling included (§4.4). `None` when the
    /// host cannot say (a v1 read, or no mode row), which admits: a device
    /// refuses only with an explicit policy, never by default.
    pub allows_cloud: Option<bool>,
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

/// The eligible root inventory of this device over the Cloud path
/// (`Store::root_export_inventory(&RootGrant::account())` after the
/// publication job): every unit that may leave, with the frontier it is at,
/// and how many units are held back — a count, never an id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootInventory {
    pub units: Vec<CloudRootUnitState>,
    pub held: u32,
}

/// What [`Host::repair_bootstrap_units`] did, and the inventory as it reads
/// afterwards (root-catalog QUESTIONS.md Q10, case 1).
///
/// The two arrive together because the repair and the read are one control
/// answer: the Store publishes the withheld units and then builds the
/// inventory, so the pass that triggered the repair can push what it just
/// created without a second round trip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootRepairOutcome {
    /// How many Workspaces gained the unit the v1 window withheld. A count,
    /// never an id — this module logs ids only for the units it moves.
    pub repaired: u32,
    /// The eligible inventory read after the repair, in the same answer.
    pub inventory: RootInventory,
}

/// The hosted account reads a Cloud pass makes. Every one is bounded, uses
/// the existing native bearer and returns `no-store` metadata; none of them
/// carries content, names or worker secrets.
///
/// The first four are the v1 pair path, kept for the compatibility window
/// (root-catalog Plan 3.2); the rest are Plan 3.1's v2 contract.
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

    /// `PUT /v1/account/cloud/catalog/binding` with what this device holds.
    /// Idempotent by contract, so an ambiguous answer is retried with the
    /// same request. [`Failure::RouteNotFound`] is a service that does not
    /// speak v2; [`Failure::CatalogBindingConflict`] is terminal.
    fn bind_catalog(
        &mut self,
        authority: &EnrollmentAuthorization,
        held: Option<CloudCatalogBindingRefV2>,
    ) -> Result<CloudCatalogBindingV2, Failure>;

    /// `GET /v1/account/cloud/catalog`: the account's admitted units with
    /// their frontiers; `after` continues a paged answer.
    fn root_catalog(
        &mut self,
        authority: &EnrollmentAuthorization,
        after: Option<CloudUnitRefV2>,
    ) -> Result<CloudRootCatalogV2, Failure>;

    /// `POST /v1/account/cloud/catalog/units/pull` from this device's
    /// accepted frontier (empty asks for a snapshot).
    fn pull_root_unit(
        &mut self,
        authority: &EnrollmentAuthorization,
        unit: CloudUnitRefV2,
        frontier: Vec<u8>,
    ) -> Result<CloudRootUnitUpdatesV2, Failure>;

    /// `POST /v1/account/cloud/catalog/units/push` with derived bytes.
    fn push_root_unit(
        &mut self,
        authority: &EnrollmentAuthorization,
        updates: CloudRootUnitUpdatesV2,
    ) -> Result<CloudRootJoinReceiptV2, Failure>;

    /// `GET /v1/account/cloud/discovery?scope=workspaces`: one page of the
    /// committed projection, grouped by current placement.
    fn discover_v2(
        &mut self,
        authority: &EnrollmentAuthorization,
        after: Option<CloudDiscoveryCursorV2>,
    ) -> Result<CloudDiscoveryReceiptV2, Failure>;

    /// `POST /v1/account/cloud/sync-token` for one scope. The response is
    /// validated for shape by the client and correlated here.
    fn issue_token_v2(
        &mut self,
        authority: &EnrollmentAuthorization,
        scope: CloudSyncScopeV2,
    ) -> Result<SyncTokenResponseV2, Failure>;

    /// `GET /v1/account/cloud/workspaces/{workspace_id}/descriptor`, by
    /// Workspace id alone. [`Failure::NotReady`] is the 503.
    fn describe_v2(
        &mut self,
        authority: &EnrollmentAuthorization,
        workspace: WorkspaceId,
    ) -> Result<CloudWorkspaceDescriptorV2, Failure>;
}

/// Every platform effect. The desktop adapter reaches the Store through the
/// gateway's typed client over the existing control channel; the Android
/// adapter reaches the same Store ports in process through `StoreWorkerClient`.
/// Neither is implemented here: this module never opens a Store file, dials a
/// route or frames a control message.
pub trait Host: Send + Sync + 'static {
    /// Does this device already hold the Workspace with a manifest identity?
    /// This is the originating-device branch of HOST-CONTROL §3.1
    /// (`read_cloud_bootstrap`): the account and the issued Workspace
    /// dataset are the verified context the desktop adapter sends over the
    /// control channel, and the Android adapter checks in process. `dataset`
    /// is always the Workspace arm; a Profile dataset is refused by every
    /// host. No Profile is asked about (TECHNICAL.final §7.3).
    fn local_workspace(
        &self,
        account: KoruIdentityId,
        dataset: CloudReplicaPrincipalV1,
    ) -> Result<Option<LocalBootstrap>, Failure>;

    /// **The Profiles of this account that are on this device** — chosen at
    /// sign-in, created here, or brought here from the Koru Cloud page.
    ///
    /// *Sign in, see the account's Profiles, sync the ones you pick.* The
    /// service answers for every Workspace the account may sync; this is what
    /// narrows the pass to the ones the person asked for. A Workspace placed
    /// under a Profile that is not here is not asked for at all, and a
    /// refused Workspace is answered by selecting its Profile only when that
    /// Profile is here. It is the device's rule, not the service's: the
    /// service no longer requires a device to be bound to a Profile before
    /// it issues a credential, so nothing else keeps a Profile the person
    /// did not choose off this device.
    ///
    /// `None` is a host that keeps no such list, and it is not limited:
    /// every Workspace the service lists is admitted, as before.
    fn held_profiles(
        &self,
        _account: KoruIdentityId,
    ) -> Option<std::collections::BTreeSet<ProfileId>> {
        None
    }

    /// `Store::provision_account_profile(id, account binding, name, locale,
    /// now)`, or the check that it already ran. Since Plan 3.2 this is a
    /// prerequisite of a **Profile-units** admission only — the Profile
    /// whose Identity/Private units are about to sync must exist here — and
    /// of the v1 pair path. It is never asked before a Workspace's content.
    fn ensure_profile_presence(
        &self,
        profile: ProfileId,
        account: KoruIdentityId,
    ) -> Result<(), Failure>;

    /// Bootstrap step 3 for a Workspace this device does not hold, from the
    /// Workspace permit about to be installed: `Store::provision_replica_placed`
    /// with `ReplicaPlacement::KnownOnly` — the placement comes from the
    /// Workspace's root unit, joined earlier in the pass, never from the
    /// descriptor; a placement this device cannot list under yet is
    /// [`Failure::PlacementPending`] and the pass retries. Only a permit of
    /// the v1 window (`catalog_scope_id: None`, a service with no root units
    /// to join) seeds the placement from the descriptor's Profile, as the
    /// pre-M3 bootstrap did. No placeholder Profile is ever constructed.
    fn ensure_workspace_presence(&self, permit: &CloudPermitV2) -> Result<(), Failure>;

    /// HOST-CONTROL §3 `replace_cloud_context`. The sequence is strictly
    /// increasing within one coordinator session; a restart invalidates it.
    /// Every permit here has a dataset: the Workspace and Profile-units
    /// scopes. The user-catalog and user-personal admissions never reach a
    /// host, because no bridged stream runs under them yet (the contract's
    /// item 3): their units go over the account API from the coordinator.
    fn replace_context(
        &self,
        sequence: u64,
        account: KoruIdentityId,
        account_generation: u64,
        permits: &[CloudPermitV2],
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
        permit: &CloudPermitV2,
        credential: &SyncCredential,
        route: &CloudSyncRouteV1,
        dataset: CloudReplicaPrincipalV1,
        keys: &JwksResponseV1,
        access: &SessionAccess,
    ) -> Result<SessionReport, Failure>;

    // ----- The Store half of the account path (root-catalog Plan 3.2) -----

    /// `Store::account_binding`: what this device declares to the binding
    /// route. `None` for a device that has never bound.
    fn account_binding(&self) -> Result<Option<CloudAccountBindingRef>, Failure>;

    /// `Store::bind_account_scope` with the service's verified answer. The
    /// same binding again is a no-op; a device bound to another account is
    /// [`Failure::CatalogBindingConflict`] and the local catalog is left
    /// exactly as it was.
    fn bind_account_scope(&self, binding: &CloudCatalogBindingV2) -> Result<(), Failure>;

    /// The publication job, then `Store::root_export_inventory` over the
    /// account's Cloud grant.
    fn root_inventory(&self) -> Result<RootInventory, Failure>;

    /// **Q10's repair**: `Store::repair_bootstrap_units`, then the
    /// publication job and [`Self::root_inventory`] again, in one answer.
    ///
    /// Publishes the Workspace unit the v1 Cloud window withheld
    /// (`ReplicaPlacement::HintWithoutUnit`, QUESTIONS.md Q7) for every live
    /// Workspace this device holds with no unit at all, placed by the hint the
    /// row still projects, with a **fresh** move id.
    ///
    /// **The caller owns the precondition and it is not negotiable**: this is
    /// asked only once the pass has run under [`CloudContract::V2`], read the
    /// service's **full** inventory — the pagination loop ended on an absent
    /// cursor, not on a cancellation — and finished its pulls. Only then does
    /// a still-unit-less row prove no unit exists on the service or here.
    /// Asked earlier it would publish a placement that a real unit still in
    /// flight could race, which is the hazard Q7 was decided to remove. The
    /// account-scope binding is **not** a substitute for the contract: it is
    /// never unset by a 404, so a bound device can still run the v1 path for a
    /// generation (ROLLOUT.md).
    ///
    /// It is carried to the Store as an optional field on the control request
    /// the inventory read already uses, not as a new request variant an older
    /// gateway would refuse.
    fn repair_bootstrap_units(&self) -> Result<RootRepairOutcome, Failure>;

    /// The frontier this device holds for one unit (`Store::root_unit_frontier`
    /// / `personal_frontier` over the Cloud path), or `None` when it holds
    /// no such unit. A subject whose mode blocks Cloud inbound is a refusal.
    fn root_unit_frontier(&self, unit: JoinUnit) -> Result<Option<Vec<u8>>, Failure>;

    /// One unit through the export boundary for the account (`Store::
    /// root_export_source` → `derive` → the policy recheck), from the
    /// service's frontier. A hold is answered as a hold, never as raw bytes.
    fn export_root_unit(
        &self,
        unit: JoinUnit,
        recipient_frontier: Option<&[u8]>,
    ) -> Result<CloudRootExport, Failure>;

    /// `Store::stage_root_unit` + `commit_staged_root_unit` (or the personal
    /// pair) with the account id as the relationship; the Store writes the
    /// join receipt. A candidate whose premises moved is retried next pass.
    fn join_root_unit(
        &self,
        account: KoruIdentityId,
        unit: JoinUnit,
        frames: &[Vec<u8>],
        advertised: Option<&[u8]>,
    ) -> Result<CloudRootJoin, Failure>;
}

mod credentials;
pub use credentials::SessionAccess;

/// One live permit's current in-memory state. This is HOST-CONTROL §3's
/// `CloudStatus` record: no credential string, name, title or content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusRecord {
    pub permit_id: Uuid,
    /// The Profile the permit names as a placement hint: where a Workspace
    /// is currently placed (`None` when explicitly unassigned), or the
    /// Profile whose units a Profile-units permit admits.
    pub profile_id: Option<ProfileId>,
    pub account_id: KoruIdentityId,
    pub account_generation: u64,
    pub dataset: CloudReplicaPrincipalV1,
    pub stage: Stage,
    pub error_code: Option<&'static str>,
    pub pending_consent_ids: Vec<Uuid>,
    pub bytes_held: u64,
    pub queued_jobs: u32,
}

/// A subject the pass could not admit: a Workspace, grouped under its current
/// placement, or a Profile whose units could not be admitted (no
/// Workspace). Diagnostics for the host surface, held in memory for this
/// pass only; it is not part of `CloudStatus` and never input to the
/// coverage projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deferred {
    pub profile_id: Option<ProfileId>,
    pub workspace_id: Option<WorkspaceId>,
    pub code: &'static str,
    /// The root unit this row is about, for the two terminal per-unit
    /// refusals (QUESTIONS.md Q12 (a), (b)): the id is the whole point of
    /// the row, because nothing else records *which* unit the service
    /// refused. `None` on a deferred subject, which is named by the ids
    /// above.
    ///
    /// A row for a refusal of Q12 (b) carries **no** `profile_id`: there is
    /// nothing a person can do about it, so it must not relabel a Profile's
    /// connection — it is a diagnostic, not a state.
    pub unit: Option<CloudUnitRefV2>,
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
    /// Which contract the service answered this generation with; `None`
    /// until the binding route has been asked.
    pub contract: Option<CloudContract>,
    /// The account's catalog scope as the service minted it, once bound.
    pub catalog_scope_id: Option<CatalogScopeId>,
}

mod discovery;
pub use discovery::{Coverage, DiscoveryCounts, DiscoveryReceipt};

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
