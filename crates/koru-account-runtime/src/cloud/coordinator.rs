// SPDX-License-Identifier: MPL-2.0

//! One bounded pass over the account's Cloud catalog.
//!
//! The order inside a pass is fixed (root-catalog TECHNICAL.final §7.3,
//! Plan 3.2): the account's catalog binding (once per account generation,
//! which also tells the pass which contract the service speaks), the
//! root-unit join over the account path — Profile units, then Workspace
//! units, pulled and pushed through the Store's own staged join and export
//! boundary — then discovery grouped by current placement, then one
//! admission per scope: a Workspace's content on its own id, membership,
//! incarnation and this device's policy; a Profile's Identity/Private units
//! only while a Cloud-enabled Workspace is currently placed under it; the
//! account's personal unit per Workspace after its content. Every step is a
//! bounded call on an injected port, and the coordinator holds no lock while
//! one runs — it owns its state exclusively on the worker thread and
//! publishes a whole snapshot after.
//!
//! The pre-root-catalog pair path (`legacy`) is kept only for a service that
//! answers the binding route with 404, and it feeds the same admission,
//! session and status machinery: there is one coordinator and one permit
//! type, not two paths side by side.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Instant;

use koru_account_core::root_catalog::{
    ClientContentPolicy, HostedContentFacts, MembershipFact, SubjectLifecycle,
    decide_content_admission,
};
use koru_account_core::workspace_lifecycle::WorkspaceCloudState;
use koru_account_wire::cloud_sync::DatasetRouteResultV1;
use koru_account_wire::{CloudDiscoveryScopeV2, CloudRouteResultV2, CloudWorkspaceCandidateV2};
use koru_store_core::profile_units::ProfileUnitRef;
use koru_sync_control::cloud::CLOUD_PERMIT_VERSION_V2;

use super::*;
use crate::ports::{Clock, Entropy};

/// A Workspace as a pass sees it, from either contract: content identity,
/// the account's membership facts and the **current placement** the
/// service's committed projection holds for it. Placement is where the
/// Workspace is grouped and what the permit carries as a hint; it is never
/// an admission input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Candidate {
    pub(super) workspace_id: WorkspaceId,
    pub(super) incarnation: Uuid,
    pub(super) placement: Option<ProfileId>,
    pub(super) placement_revision: i64,
    pub(super) lifecycle: SubjectLifecycle,
    pub(super) authorization_version: i64,
    pub(super) membership_epoch: i64,
    pub(super) cloud_access_epoch: i64,
    /// The v1 pair path only: the membership Profile the v1 routes name.
    /// `None` under the v2 contract, where no route takes one.
    pub(super) legacy_profile: Option<ProfileId>,
}

impl Candidate {
    pub(super) fn from_v1(candidate: &CloudWorkspaceCandidateV1) -> Self {
        Self {
            workspace_id: candidate.workspace_id,
            incarnation: candidate.workspace_incarnation_id,
            placement: Some(candidate.profile_id),
            placement_revision: 0,
            lifecycle: SubjectLifecycle::Live,
            authorization_version: candidate.authorization_version,
            membership_epoch: candidate.membership_epoch,
            cloud_access_epoch: candidate.cloud_access_epoch,
            legacy_profile: Some(candidate.profile_id),
        }
    }

    fn from_v2(candidate: &CloudWorkspaceCandidateV2) -> Self {
        Self {
            workspace_id: candidate.workspace_id,
            incarnation: candidate.workspace_incarnation_id,
            placement: candidate.placement.placement,
            placement_revision: candidate.placement.placement_revision,
            lifecycle: candidate.lifecycle,
            authorization_version: candidate.membership.authorization_version,
            membership_epoch: candidate.membership.membership_epoch,
            cloud_access_epoch: candidate.membership.cloud_access_epoch,
            legacy_profile: None,
        }
    }
}

/// One scope's admission: the credential its token carried, where its
/// session dials (when the scope has a dataset session at all), and the
/// permit built from it.
#[derive(Clone)]
pub(super) struct Scoped {
    pub(super) credential: SyncCredential,
    pub(super) expires_at_seconds: i64,
    pub(super) route: CloudRouteResultV2,
    pub(super) permit: CloudPermitV2,
    /// The receipt's authorization generation this admission was issued
    /// under; a receipt with a newer one fences it.
    pub(super) issued_under: u64,
}

impl Scoped {
    fn fresh(&self, now_seconds: i64) -> bool {
        self.expires_at_seconds.saturating_sub(now_seconds) > SESSION_RENEW_BEFORE_SECONDS
    }
}

/// What one Workspace reached before its content session was opened: its
/// content admission and, when the account's catalog holds a personal unit
/// for it, the user-personal admission that follows the Workspace's policy
/// (§7.3). No Profile admission is paired with it.
#[derive(Clone)]
pub(super) struct Admission {
    pub(super) candidate: Candidate,
    pub(super) content: Scoped,
    pub(super) personal: Option<Scoped>,
    /// The v1 pair path only: the pair's Profile dataset, read as a
    /// Profile-units admission and promoted to the Profile's by the first
    /// admitted pair placed under it.
    pub(super) legacy_profile_units: Option<Scoped>,
}

/// What one session lane opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Owner {
    Workspace(WorkspaceId),
    Profile(ProfileId),
}

/// One dataset session to run: the owner's scoped admission.
#[derive(Clone)]
pub(super) struct Session {
    pub(super) owner: Owner,
    pub(super) scoped: Scoped,
}

/// The identity a Workspace permit is built from, and where it came from.
pub(super) enum Identity {
    /// HOST-CONTROL §3.1: this device originated the Workspace, so the
    /// manifest id is the durable local one and the rest of the descriptor is
    /// formed from the current credential context.
    Local(LocalBootstrap),
    /// The hosted descriptor read answered a retained mapping.
    Hosted(ControlDescriptorV2),
}

/// What the v2 discovery receipt said beside its results, kept for the
/// pass's own receipt and for the Profile-units eligibility rule.
#[derive(Clone, Debug, Default)]
pub(super) struct ReceiptFacts {
    pub(super) scope: Option<CloudDiscoveryScopeV2>,
    pub(super) authorization_generation: Option<u64>,
    pub(super) coverage: Option<Coverage>,
    pub(super) lifecycles: BTreeMap<ProfileId, SubjectLifecycle>,
    /// Profiles the service's committed projection finds eligible for
    /// Identity/Private (`profile_units_eligible`, live).
    pub(super) eligible: BTreeSet<ProfileId>,
}

pub(super) struct Coordinator<A: Api, H: Host> {
    pub(super) api: A,
    pub(super) host: Arc<H>,
    entropy: Arc<dyn Entropy>,
    pub(super) cancel: Cancel,
    pub(super) authority: Option<EnrollmentAuthorization>,
    owner_scope: Option<u64>,
    /// In memory only. A replaced authorization increments it; every older
    /// permit is fenced and dropped with it.
    pub(super) account_generation: u64,
    /// Strictly increasing within this coordinator session. A restart
    /// invalidates the whole sequence space.
    pub(super) sequence: u64,
    /// The permits the host holds: Workspace content and Profile units.
    pub(super) live: Vec<CloudPermitV2>,
    pub(super) admissions: BTreeMap<WorkspaceId, Admission>,
    /// The Profiles whose Identity/Private units are independently eligible
    /// this generation, with their admission (§4.4).
    pub(super) profile_units: BTreeMap<ProfileId, Scoped>,
    /// The user-catalog admission the root-unit join runs under; never
    /// handed to the host. `None` until issued this generation.
    pub(super) catalog: Option<Scoped>,
    /// Which contract the service answered this generation with.
    pub(super) contract: Option<CloudContract>,
    pub(super) catalog_scope_id: Option<CatalogScopeId>,
    /// The service's catalog as the last root exchange read it, keyed by
    /// unit; and this device's eligible inventory. Both for the personal
    /// exchange that follows content.
    pub(super) service_units: BTreeMap<CloudUnitRefV2, Vec<u8>>,
    pub(super) local_units: BTreeMap<CloudUnitRefV2, Vec<u8>>,
    /// Units the service refused terminally, with the code it refused them
    /// with (QUESTIONS.md Q12 (a), (b)). **Asked once and then never again**:
    /// a format this build cannot read and a unit the service will not serve
    /// are not answered by asking a second time, and the pass carries on with
    /// every other unit. A replaced generation clears it, because a new
    /// authorization is a new question; a newer build clears it by existing.
    pub(super) refused_units: BTreeMap<CloudUnitRefV2, &'static str>,
    /// Q12 (c)'s ceiling: the binding step has already been run again during
    /// this pass, so a further binding-required answer is not a precondition
    /// this client can satisfy. Cleared at the top of every pass.
    pub(super) binding_retried: bool,
    pub(super) records: BTreeMap<Uuid, StatusRecord>,
    deferred: Vec<Deferred>,
    deferred_failure: Option<Failure>,
    discovery: Option<DiscoveryReceipt>,
    /// Counts by current placement; `None` is the unassigned group.
    discovery_counts: BTreeMap<Option<ProfileId>, DiscoveryCounts>,
    discovery_exhausted: bool,
    pub(super) receipt: ReceiptFacts,
    pub(super) carried: VecDeque<Candidate>,
    /// The verification material, bound to the generation it was read under.
    pub(super) jwks: Option<(u64, JwksResponseV1)>,
    pending: bool,
    retry_at: Option<Instant>,
    debounce_at: Option<Instant>,
    failures: u32,
    failure_code: Option<&'static str>,
    retry_at_millis: Option<i64>,
    revision: u64,
    pub(super) stage: Stage,
    progress: Option<Box<dyn Fn(Snapshot) + Send>>,
    desired: Option<Arc<Mutex<Desired>>>,
    foreground: Option<Arc<AtomicBool>>,
}

impl<A: Api, H: Host> Coordinator<A, H> {
    pub(super) fn new(api: A, host: H, entropy: Arc<dyn Entropy>, cancel: Cancel) -> Self {
        Self {
            api,
            host: Arc::new(host),
            entropy,
            cancel,
            authority: None,
            owner_scope: None,
            account_generation: 0,
            sequence: 0,
            live: Vec::new(),
            admissions: BTreeMap::new(),
            profile_units: BTreeMap::new(),
            catalog: None,
            contract: None,
            catalog_scope_id: None,
            service_units: BTreeMap::new(),
            local_units: BTreeMap::new(),
            refused_units: BTreeMap::new(),
            binding_retried: false,
            records: BTreeMap::new(),
            deferred: Vec::new(),
            deferred_failure: None,
            discovery: None,
            discovery_counts: BTreeMap::new(),
            discovery_exhausted: false,
            receipt: ReceiptFacts::default(),
            carried: VecDeque::new(),
            jwks: None,
            pending: false,
            retry_at: None,
            debounce_at: None,
            failures: 0,
            failure_code: None,
            retry_at_millis: None,
            revision: 0,
            stage: Stage::AwaitingAuthority,
            progress: None,
            desired: None,
            foreground: None,
        }
    }

    pub(super) fn observe(&mut self, progress: impl Fn(Snapshot) + Send + 'static) {
        self.progress = Some(Box::new(progress));
    }

    pub(super) fn follow_authorization(
        &mut self,
        desired: Arc<Mutex<Desired>>,
        foreground: Arc<AtomicBool>,
    ) {
        self.desired = Some(desired);
        self.foreground = Some(foreground);
    }

    pub(super) fn session_access(&self, authority: &EnrollmentAuthorization) -> SessionAccess {
        SessionAccess {
            authority: authority.clone(),
            desired: self.desired.clone(),
            cancel: self.cancel.clone(),
            foreground: self.foreground.clone(),
        }
    }

    pub(super) fn current_authority(&self) -> Result<EnrollmentAuthorization, Failure> {
        self.authority
            .as_ref()
            .and_then(|authority| self.session_access(authority).current())
            .ok_or(Failure::SessionExpired)
    }

    pub(super) fn publish_progress(&self) {
        if let Some(progress) = &self.progress {
            progress(self.snapshot());
        }
    }

    pub(super) fn apply(&mut self, command: Command, clock: &dyn Clock) {
        match command {
            Command::Activate(authority, owner_scope) => self.activate(authority, owner_scope),
            Command::Refresh(previous, next) => self.refresh(previous, next),
            Command::Wake => self.schedule_change(clock),
            Command::Cancel => {
                self.cancel.clear();
                self.clear_authority();
            }
        }
    }

    /// A replaced authorization is an account switch or a revocation: the
    /// generation advances, every in-memory permit is fenced and dropped, and
    /// no cohort or enumeration survives it.
    fn activate(&mut self, authority: Option<EnrollmentAuthorization>, owner_scope: Option<u64>) {
        if let Some(scope) = owner_scope {
            self.owner_scope = Some(scope);
        }
        let unchanged = match (&self.authority, &authority) {
            (Some(previous), Some(next)) => same_authority(previous, next),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            // The same session may carry a newer access token. Take it
            // without disturbing the live context.
            if let Some(next) = authority {
                self.authority = Some(next);
            }
            return;
        }
        self.replace_generation();
        self.pending = authority.is_some();
        self.authority = authority;
        self.stage = Stage::AwaitingAuthority;
        self.revision = self.revision.saturating_add(1);
    }

    /// An acknowledged same-session refresh. Continuity is explicit: the
    /// generation does not advance and the live context is untouched.
    fn refresh(&mut self, previous: EnrollmentAuthorization, next: EnrollmentAuthorization) {
        if !self
            .authority
            .as_ref()
            .is_some_and(|current| same_authority(current, &previous))
            || !same_authority(&previous, &next)
        {
            return;
        }
        self.authority = Some(next);
        // A 401 parks until fresh account authority arrives. A same-session
        // refresh must wake that work without changing its account generation.
        if self.failure_code == Some(SESSION_EXPIRED) {
            self.pending = true;
        }
    }

    fn clear_authority(&mut self) {
        self.replace_generation();
        self.authority = None;
        self.pending = false;
        self.stage = Stage::AwaitingAuthority;
        self.revision = self.revision.saturating_add(1);
    }

    /// Everything in memory belongs to one account generation. The catalog
    /// and personal admissions, which never reached the host, are fenced by
    /// being dropped here; the host's permits by the explicit fence.
    fn replace_generation(&mut self) {
        let stale: Vec<Uuid> = self.live.iter().map(|permit| permit.permit_id).collect();
        self.live.clear();
        self.admissions.clear();
        self.profile_units.clear();
        self.catalog = None;
        self.contract = None;
        self.catalog_scope_id = None;
        self.service_units.clear();
        self.local_units.clear();
        // A terminal per-unit refusal belongs to the authorization it was
        // answered under: a new generation asks once more, and only once.
        self.refused_units.clear();
        self.records.clear();
        self.deferred.clear();
        self.deferred_failure = None;
        self.discovery = None;
        self.discovery_counts.clear();
        self.discovery_exhausted = false;
        self.receipt = ReceiptFacts::default();
        self.carried.clear();
        self.jwks = None;
        self.retry_at = None;
        self.debounce_at = None;
        self.failures = 0;
        self.failure_code = None;
        self.retry_at_millis = None;
        self.account_generation = self.account_generation.saturating_add(1);
        if !stale.is_empty() {
            let _ = self.host.fence(&stale);
        }
    }

    /// TECHNICAL §8's 500 ms trailing debounce. Each change or remote hint
    /// moves the next pass to 500 ms after the last event; a backoff retry
    /// that is already due is not pushed out by it.
    fn schedule_change(&mut self, clock: &dyn Clock) {
        self.debounce_at = clock.monotonic().checked_add(CHANGE_DEBOUNCE);
    }

    pub(super) fn deadline(&self, foreground: bool) -> Option<Instant> {
        if !foreground || self.authority.is_none() || self.cancel.cancelled() {
            return None;
        }
        if self.pending {
            return Some(Instant::now());
        }
        match (self.retry_at, self.debounce_at) {
            (Some(retry), Some(debounce)) => Some(retry.min(debounce)),
            (value, None) | (None, value) => value,
        }
    }

    pub(super) fn dispatch(&mut self, clock: &dyn Clock, foreground: bool) {
        if !foreground {
            self.pending = self.authority.is_some();
        }
        if !foreground && !self.live.is_empty() {
            self.replace_generation();
            self.pending = self.authority.is_some();
        }
        if !foreground || self.authority.is_none() || self.cancel.cancelled() {
            return;
        }
        let now = clock.monotonic();
        let due = self.pending
            || self.retry_at.is_some_and(|at| at <= now)
            || self.debounce_at.is_some_and(|at| at <= now);
        if !due {
            return;
        }
        self.pending = false;
        self.retry_at = None;
        self.debounce_at = None;
        self.run_pass(clock);
    }

    pub(super) fn snapshot(&self) -> Snapshot {
        let Some(authority) = self.authority.as_ref() else {
            return Snapshot {
                owner_scope: self.owner_scope,
                revision: self.revision,
                account_generation: self.account_generation,
                ..Snapshot::idle()
            };
        };
        Snapshot {
            version: 1,
            revision: self.revision,
            owner_scope: self.owner_scope,
            account_id: Some(authority.owner.account),
            account_generation: self.account_generation,
            stage: self.stage,
            records: self.records.values().cloned().collect(),
            deferred: self.deferred.clone(),
            failure_code: self.failure_code,
            retry_at_millis: self.retry_at_millis,
            carried: self.carried.len(),
            sequence: self.sequence,
            discovery: self.discovery.clone(),
            contract: self.contract,
            catalog_scope_id: self.catalog_scope_id,
        }
    }

    pub(super) fn mint(&self) -> Uuid {
        let mut bytes = [0_u8; 16];
        self.entropy.fill_bytes(&mut bytes);
        uuid::Builder::from_random_bytes(bytes).into_uuid()
    }

    /// The receipt's authorization generation admissions are issued under
    /// this pass; 1 on the v1 path, which has none.
    pub(super) fn receipt_generation(&self) -> u64 {
        self.receipt.authorization_generation.unwrap_or(1)
    }

    /// A v2 permit for one scope from the token that admitted it.
    pub(super) fn permit_v2(
        &self,
        authority: &EnrollmentAuthorization,
        token: &SyncTokenResponseV2,
        scope: CloudPermitScopeV2,
    ) -> Result<CloudPermitV2, Failure> {
        let permit = CloudPermitV2 {
            version: CLOUD_PERMIT_VERSION_V2,
            permit_id: self.mint(),
            account_id: cloud_id(authority.owner.account.as_uuid()),
            catalog_scope_id: Some(cloud_id(token.catalog_scope_id.as_uuid())),
            scope,
            account_generation: self.account_generation,
            authority_generation: token.authority_generation,
            expires_at_seconds: token.expires_at_seconds,
        };
        permit.validate().map_err(|_| Failure::DatasetMismatch)?;
        Ok(permit)
    }

    /// Is the record of a live permit in a state its admission may be kept
    /// across passes? An errored or unavailable record is re-admitted.
    pub(super) fn healthy(&self, permit_id: Uuid) -> bool {
        self.records.get(&permit_id).is_some_and(|record| {
            record.error_code.is_none()
                && matches!(
                    record.stage,
                    Stage::Settled | Stage::CheckingCoverage | Stage::Reconciling
                )
        })
    }

    // ----- one pass -------------------------------------------------------

    fn run_pass(&mut self, clock: &dyn Clock) {
        let Some(authority) = self.authority.clone() else {
            return;
        };
        self.revision = self.revision.saturating_add(1);
        self.deferred.clear();
        self.deferred_failure = None;
        // Q12 (c): one run of the binding step per pass. The next pass comes
        // behind the backoff, which is the outer retry a service that keeps
        // losing the binding row is answered with.
        self.binding_retried = false;
        self.discovery_counts.clear();
        self.discovery_exhausted = false;
        for candidate in &self.carried {
            self.discovery_counts
                .entry(candidate.placement)
                .or_default()
                .discovered += 1;
        }
        if let Some(receipt) = &mut self.discovery {
            receipt.stale = true;
        }
        if self.live.is_empty() {
            self.stage = Stage::Connecting;
        }
        self.failure_code = None;
        self.retry_at_millis = None;
        self.publish_progress();
        let mut failure: Option<Failure> = None;

        if self.carried.is_empty() {
            // 1. The account's one catalog binding, once per generation; it
            //    also says which contract the service speaks. A refusal of
            //    the binding is terminal: nothing of this account is
            //    admitted under it.
            if let Err(error) = self.ensure_contract() {
                self.stage = stage_for(&error);
                self.settle(clock, Some(error));
                return;
            }
            // 2. The root-unit join: Profile units, then Workspace units,
            //    before any Workspace is admitted — so the placement a
            //    bootstrap needs is here when the content arrives. A failed
            //    join is reported and retried; content is not held hostage
            //    to it (§9: unrelated work is not stalled).
            if self.contract == Some(CloudContract::V2)
                && let Err(error) = self.exchange_root_units(clock)
            {
                if !error.automatic() {
                    self.stage = stage_for(&error);
                    self.settle(clock, Some(error));
                    return;
                }
                failure = Some(merge(failure, error));
            }
            // 3. Discovery, grouped by current placement.
            if let Err(error) = self.enumerate() {
                // Without a catalog there is nothing to admit this pass. The
                // live context is left alone; a stale permit is fenced only
                // by an authority change or by the next successful
                // enumeration.
                self.stage = stage_for(&error);
                self.settle(clock, Some(merge(failure, error)));
                return;
            }
        }
        if let Err(error) = self.refresh_verification() {
            self.stage = stage_for(&error);
            self.settle(clock, Some(merge(failure, error)));
            return;
        }

        let mut cohort = VecDeque::with_capacity(self.carried.len());
        while cohort.len() < MAX_CARRIED_CANDIDATES {
            match self.carried.pop_front() {
                Some(candidate) => cohort.push_back(candidate),
                None => break,
            }
        }

        let now_seconds = clock.now_millis().unwrap_or(i64::MAX) / 1000;
        let generation = self.receipt_generation();
        let mut admitted = Vec::with_capacity(cohort.len());
        while let Some(candidate) = cohort.pop_front() {
            if self.cancel.cancelled() {
                self.carried.push_front(candidate);
                self.carried.extend(cohort);
                break;
            }
            // A cached admission is kept while the candidate is the same —
            // placement revision included, so a move re-admits — its
            // permits are healthy and fresh, and the receipt's authorization
            // generation has not moved under it.
            let cached = self
                .admissions
                .get(&candidate.workspace_id)
                .filter(|previous| {
                    previous.candidate == candidate
                        && previous.content.issued_under == generation
                        && self.healthy(previous.content.permit.permit_id)
                        && previous.content.fresh(now_seconds)
                        && previous
                            .personal
                            .as_ref()
                            .is_none_or(|personal| personal.fresh(now_seconds))
                })
                .cloned();
            let admission = match cached {
                Some(admission) => Ok(Some(admission)),
                None => self.admit(&authority, candidate.clone()),
            };
            match admission {
                Ok(Some(admission)) => admitted.push(admission),
                Ok(None) => {}
                Err(error) => {
                    self.defer(&candidate, error.clone());
                    failure = Some(merge(failure, error));
                }
            }
        }

        // 4. Profile Identity/Private admissions, independently eligible
        //    (§4.4): recomputed from this pass's receipt and admissions, so
        //    a Profile whose last Cloud Workspace moved out loses its
        //    admission and the destination gains one at the same revision.
        self.reconcile_profile_units(&authority, &admitted, clock);

        // The standing permit ceiling is a hard limit, not a truncation of an
        // acknowledged context: a Workspace over the budget is deferred whole.
        while admitted.len() + self.profile_units.len() > MAX_LIVE_PERMITS {
            if let Some(dropped) = admitted.pop() {
                self.defer(&dropped.candidate, Failure::Unavailable);
            } else {
                break;
            }
        }

        let retained: BTreeSet<Uuid> = admitted
            .iter()
            .map(|admission| admission.content.permit.permit_id)
            .chain(
                self.profile_units
                    .values()
                    .map(|scoped| scoped.permit.permit_id),
            )
            .collect();
        let stale: Vec<Uuid> = self
            .live
            .iter()
            .map(|permit| permit.permit_id)
            .filter(|id| !retained.contains(id))
            .collect();
        if !stale.is_empty()
            && let Err(error) = self.host.fence(&stale)
        {
            self.settle(clock, Some(merge(failure, error)));
            return;
        }
        self.live
            .retain(|permit| retained.contains(&permit.permit_id));
        self.admissions
            .retain(|_, admission| retained.contains(&admission.content.permit.permit_id));
        self.records.retain(|id, _| retained.contains(id));

        let permits: Vec<CloudPermitV2> = admitted
            .iter()
            .map(|admission| admission.content.permit.clone())
            .chain(
                self.profile_units
                    .values()
                    .map(|scoped| scoped.permit.clone()),
            )
            .collect();
        for admission in &admitted {
            self.discovery_counts
                .entry(admission.candidate.placement)
                .or_default()
                .admitted += 1;
            self.admissions
                .insert(admission.candidate.workspace_id, admission.clone());
        }
        let Ok(authority) = self.current_authority() else {
            return;
        };
        // The account's reach into the blob control plane, before the permits
        // that name the Workspaces it is for. A host that cannot take it says
        // so and the pass carries on: documents are not blocked on pictures.
        if let Err(error) = self.host.replace_blob_access(
            authority.owner.account,
            self.account_generation,
            &authority.access,
        ) {
            failure = Some(merge(failure, error));
        }
        self.sequence = self.sequence.saturating_add(1);
        // Retain ownership before a fallible acknowledgement: the host may
        // apply the page even when its reply is lost.
        self.live = permits.clone();
        if let Err(error) = self.host.replace_context(
            self.sequence,
            authority.owner.account,
            self.account_generation,
            &permits,
        ) {
            self.stage = stage_for(&error);
            self.settle(clock, Some(merge(failure, error)));
            return;
        }
        for permit in &permits {
            self.records
                .entry(permit.permit_id)
                .or_insert_with(|| record_for(permit, authority.owner.account));
        }
        self.live = permits;

        let sessions: Vec<Session> = admitted
            .iter()
            .map(|admission| Session {
                owner: Owner::Workspace(admission.candidate.workspace_id),
                scoped: admission.content.clone(),
            })
            .chain(self.profile_units.iter().map(|(profile, scoped)| Session {
                owner: Owner::Profile(*profile),
                scoped: scoped.clone(),
            }))
            .collect();
        let hinted = self.run_sessions(sessions, clock, &mut failure);

        // 5. Personal units after content (§7.3: they follow the Workspace's
        //    policy plus the account's authorization, never a Profile).
        if self.contract == Some(CloudContract::V2)
            && let Err(error) = self.exchange_personal_units(clock)
        {
            failure = Some(merge(failure, error));
        }

        self.stage = self
            .records
            .values()
            .map(|record| record.stage)
            .max_by_key(|stage| stage.severity())
            .unwrap_or_else(|| failure.as_ref().map_or(Stage::Settled, stage_for));
        self.settle(clock, failure);
        if hinted || !self.carried.is_empty() {
            self.schedule_change(clock);
        }
    }

    /// Pages to the end of the catalog. Exhaustion is explicit: repeating a
    /// truncated enumeration must not silently starve later Workspaces.
    fn enumerate(&mut self) -> Result<(), Failure> {
        let candidates = match self.contract {
            Some(CloudContract::V2) => self.enumerate_v2()?,
            _ => self.enumerate_v1()?,
        };
        // Round robin by current placement before taking a bounded cohort:
        // a large Profile cannot occupy all sixteen slots ahead of every
        // other Profile, and the unassigned group is one group among them.
        let mut groups = BTreeMap::<Option<ProfileId>, VecDeque<Candidate>>::new();
        for candidate in candidates {
            groups
                .entry(candidate.placement)
                .or_default()
                .push_back(candidate);
        }
        self.carried.clear();
        while groups.values().any(|queue| !queue.is_empty()) {
            for queue in groups.values_mut() {
                if let Some(candidate) = queue.pop_front() {
                    self.carried.push_back(candidate);
                }
            }
        }
        Ok(())
    }

    /// `GET /v1/account/cloud/discovery?scope=workspaces`, paged: results
    /// grouped by current placement, the Profiles' lifecycles and
    /// eligibility, and the receipt's coverage. A receipt for another
    /// account is refused, never correlated.
    fn enumerate_v2(&mut self) -> Result<VecDeque<Candidate>, Failure> {
        let account = self.current_authority()?.owner.account;
        let mut after = None;
        let mut candidates = VecDeque::new();
        let mut facts = ReceiptFacts::default();
        loop {
            if self.cancel.cancelled() {
                break;
            }
            let receipt = self.authorized(|api, authority| api.discover_v2(authority, after))?;
            if receipt.account_id != account
                || self
                    .catalog_scope_id
                    .is_some_and(|scope| scope != receipt.catalog_scope_id)
            {
                return Err(Failure::DatasetMismatch);
            }
            facts.scope = Some(receipt.scope);
            facts.authorization_generation = Some(receipt.authorization_generation);
            facts.coverage = Some(Coverage::from_wire(&receipt.coverage));
            for profile in &receipt.profiles {
                facts
                    .lifecycles
                    .insert(profile.profile_id, profile.lifecycle);
                if profile.profile_units_eligible && profile.lifecycle == SubjectLifecycle::Live {
                    facts.eligible.insert(profile.profile_id);
                }
            }
            for result in &receipt.results {
                let candidate = Candidate::from_v2(result);
                self.discovery_counts
                    .entry(candidate.placement)
                    .or_default()
                    .discovered += 1;
                if candidates.len() >= MAX_CARRIED_CANDIDATES {
                    tracing::warn!(
                        capacity = MAX_CARRIED_CANDIDATES,
                        "cloud subscriptions: account exceeds standing capacity"
                    );
                    return Err(Failure::Unavailable);
                }
                candidates.push_back(candidate);
            }
            match receipt.next {
                Some(cursor) => after = Some(cursor),
                None => {
                    self.discovery_exhausted = true;
                    break;
                }
            }
        }
        if self.discovery_exhausted {
            self.receipt = facts;
        }
        Ok(candidates)
    }

    /// One JWKS read per account generation, before the first credential of
    /// that generation is issued. Keys read under a replaced authority are
    /// never carried forward.
    fn refresh_verification(&mut self) -> Result<(), Failure> {
        if self
            .jwks
            .as_ref()
            .is_some_and(|(generation, _)| *generation == self.account_generation)
        {
            return Ok(());
        }
        let jwks = self.api.jwks()?;
        self.jwks = Some((self.account_generation, jwks));
        Ok(())
    }

    fn defer(&mut self, candidate: &Candidate, error: Failure) {
        let code = error.code();
        self.deferred_failure = Some(merge(self.deferred_failure.take(), error));
        let counts = self
            .discovery_counts
            .entry(candidate.placement)
            .or_default();
        counts.deferred += 1;
        counts.deferred_code.get_or_insert(code);
        // Which workspace, and why, or the only trace of a deferred dataset
        // is a counter. Ids and a stable code; nothing a person wrote.
        tracing::info!(
            target: "koru_cloud",
            workspace = ?candidate.workspace_id,
            code,
            "cloud pass: deferred a workspace"
        );
        self.deferred.push(Deferred {
            profile_id: candidate.placement,
            workspace_id: Some(candidate.workspace_id),
            code,
            unit: None,
        });
    }

    fn defer_profile(&mut self, profile: ProfileId, error: Failure) {
        let code = error.code();
        self.deferred_failure = Some(merge(self.deferred_failure.take(), error));
        self.discovery_counts
            .entry(Some(profile))
            .or_default()
            .deferred_code
            .get_or_insert(code);
        tracing::info!(
            target: "koru_cloud",
            profile = ?profile,
            code,
            "cloud pass: deferred a Profile's units"
        );
        self.deferred.push(Deferred {
            profile_id: Some(profile),
            workspace_id: None,
            code,
            unit: None,
        });
    }

    /// A unit the service refused terminally (QUESTIONS.md Q12 (a), (b)):
    /// remember it so nothing asks again, record it in sync diagnostics **by
    /// unit id**, and let the pass carry on with everything else.
    ///
    /// **It is never merged into the pass's failure.** One unit must not
    /// stop a pass, and a terminal refusal that became the pass's failure
    /// would stop the account's retry of every other unit — the opposite of
    /// what was decided.
    ///
    /// (a) is the one refusal a person can answer, so where the pass can
    /// attribute it to a Profile it does: a Profile's own unit is that
    /// Profile's, and a Workspace's unit is the placement this device last
    /// admitted the Workspace under. That Profile's row then says *Update
    /// Koru to sync this*. A Workspace this device has never admitted has no
    /// placement yet, so the first pass records the id and the prompt
    /// follows once the placement is known.
    ///
    /// (b) carries no Profile at all, by decision: there is nothing a person
    /// can do about it, so it must not relabel a Profile's connection. The
    /// id in the row is what makes it answerable later.
    pub(super) fn defer_unit(&mut self, unit: CloudUnitRefV2, error: &Failure) {
        let code = error.code();
        self.refused_units.insert(unit, code);
        let placement = match (error, unit) {
            (Failure::UnitFormatTooNew, CloudUnitRefV2::ProfileCatalog { subject_id }) => {
                Some(subject_id)
            }
            (
                Failure::UnitFormatTooNew,
                CloudUnitRefV2::WorkspaceCatalog { subject_id }
                | CloudUnitRefV2::PersonalV2 { subject_id },
            ) => self
                .admissions
                .get(&subject_id)
                .and_then(|admission| admission.candidate.placement),
            _ => None,
        };
        if let Some(profile) = placement {
            self.discovery_counts
                .entry(Some(profile))
                .or_default()
                .deferred_code
                .get_or_insert(code);
        }
        tracing::info!(
            target: "koru_cloud",
            unit = ?unit,
            code,
            "cloud pass: the service refused this unit for good"
        );
        self.deferred.push(Deferred {
            profile_id: placement,
            workspace_id: match unit {
                CloudUnitRefV2::WorkspaceCatalog { subject_id }
                | CloudUnitRefV2::PersonalV2 { subject_id } => Some(subject_id),
                CloudUnitRefV2::ProfileCatalog { .. } => None,
            },
            code,
            unit: Some(unit),
        });
    }

    fn settle(&mut self, clock: &dyn Clock, failure: Option<Failure>) {
        // A cancelled/replaced pass cannot turn its stale receipt current.
        if self.cancel.cancelled() || self.current_authority().is_err() {
            return;
        }
        let failure = match self.deferred_failure.take() {
            Some(deferred) => Some(merge(failure, deferred)),
            None => failure,
        };
        // Q12 (a), (b) are answered where the unit is asked for and are not
        // merged into a pass's failure. One that reaches here came from a
        // route that is not about a single unit, so it says nothing about a
        // unit: stopping the account's retry on it would strand every other
        // unit on one route's answer.
        let failure = failure.map(not_per_unit);
        let automatic_retry = failure.as_ref().is_some_and(Failure::automatic);
        match failure {
            None => {
                self.failures = 0;
                self.failure_code = None;
                self.retry_at_millis = None;
                self.retry_at = None;
            }
            Some(error) => {
                let delay = match &error {
                    Failure::RateLimited(after) => backoff(self.failures).max(*after),
                    _ => backoff(self.failures),
                };
                self.failures = self.failures.saturating_add(1);
                self.failure_code = Some(error.code());
                if error.automatic() {
                    self.retry_at = clock.monotonic().checked_add(delay);
                    self.retry_at_millis = checked_retry_at(clock.now_millis().unwrap_or(0), delay);
                } else {
                    self.retry_at = None;
                    self.retry_at_millis = None;
                }
            }
        }
        for counts in self.discovery_counts.values_mut() {
            counts.carried = 0;
        }
        for candidate in &self.carried {
            self.discovery_counts
                .entry(candidate.placement)
                .or_default()
                .carried += 1;
        }
        let mut profiles = BTreeMap::new();
        let mut unassigned = DiscoveryCounts::default();
        for (placement, counts) in &self.discovery_counts {
            match placement {
                Some(profile) => {
                    profiles.insert(*profile, counts.clone());
                }
                None => unassigned = counts.clone(),
            }
        }
        self.discovery = self.authority.as_ref().map(|authority| DiscoveryReceipt {
            account_id: authority.owner.account,
            pass: self.revision,
            stale: false,
            exhausted: self.discovery_exhausted,
            profiles,
            unassigned,
            failure_code: self.failure_code,
            retry_at_millis: self.retry_at_millis,
            automatic_retry,
            scope: self.receipt.scope,
            authorization_generation: self.receipt.authorization_generation,
            coverage: self.receipt.coverage,
            lifecycles: self.receipt.lifecycles.clone(),
        });
    }

    /// One Workspace's admission under whichever contract the service
    /// speaks. `Ok(None)` means the pass was cancelled part-way; a refusal is
    /// an `Err` the caller records as a deferred Workspace. Nothing is
    /// invented for either.
    pub(super) fn admit(
        &mut self,
        authority: &EnrollmentAuthorization,
        candidate: Candidate,
    ) -> Result<Option<Admission>, Failure> {
        match self.contract {
            Some(CloudContract::V2) => self.admit_v2(authority, candidate),
            _ => self.admit_v1(authority, candidate),
        }
    }

    /// §7.3's content admission: the Workspace's own token, the device's
    /// own manifest identity or the hosted descriptor by Workspace id,
    /// `decide_content_admission` over membership, lifecycle, incarnation
    /// and this device's policy — no Profile asked about, none required —
    /// then the user-personal admission beside it when the account's
    /// catalog holds a personal unit for the Workspace.
    fn admit_v2(
        &mut self,
        authority: &EnrollmentAuthorization,
        candidate: Candidate,
    ) -> Result<Option<Admission>, Failure> {
        let workspace = candidate.workspace_id;
        let token = self.authorized(|api, authority| {
            api.issue_token_v2(
                authority,
                CloudSyncScopeV2::Workspace {
                    workspace_id: workspace,
                },
            )
        })?;
        let Some(
            dataset @ CloudReplicaPrincipalV1::Workspace {
                workspace_id,
                incarnation_id,
                logical_replica_id,
            },
        ) = token.dataset
        else {
            return Err(Failure::DatasetMismatch);
        };
        if workspace_id != cloud_id(workspace.as_uuid())
            || incarnation_id != cloud_id(candidate.incarnation)
        {
            return Err(Failure::DatasetMismatch);
        }
        if self.cancel.cancelled() {
            return Ok(None);
        }
        let local = self
            .host
            .local_workspace(authority.owner.account, dataset)?;
        let identity = match local {
            Some(local) => {
                if local.workspace_id != workspace {
                    return Err(Failure::DatasetMismatch);
                }
                Identity::Local(local)
            }
            // A 503 is "not ready": skip the Workspace this pass and retry
            // with backoff rather than inventing an identity for it.
            None => {
                let descriptor =
                    self.authorized(|api, authority| api.describe_v2(authority, workspace))?;
                correlate_v2(&candidate, &descriptor, dataset)?;
                Identity::Hosted(control_descriptor_v2(&descriptor))
            }
        };
        // Membership, lifecycle, incarnation, local policy — in that order,
        // and nothing about a Profile. The Profile the Workspace is placed
        // under may be tombstoned or absent here; its content still syncs.
        let policy = ClientContentPolicy {
            expected_incarnation_id: None,
            allows_cloud: match &identity {
                Identity::Local(local) => local.allows_cloud.unwrap_or(true),
                Identity::Hosted(_) => true,
            },
        };
        decide_content_admission(
            authority.owner.account,
            &hosted_facts(authority.owner.account, &candidate, logical_replica_id),
            policy,
        )
        .map_err(Failure::Content)?;
        let hosted = matches!(identity, Identity::Hosted(_));
        let descriptor = match identity {
            Identity::Hosted(descriptor) => descriptor,
            Identity::Local(local) => ControlDescriptorV2 {
                version: koru_sync_control::cloud::CLOUD_WORKSPACE_DESCRIPTOR_VERSION_V2,
                workspace_id,
                workspace_incarnation_id: incarnation_id,
                logical_replica_id,
                lineage_id: incarnation_id,
                manifest_document_id: cloud_id(local.manifest_document_id),
                authorization_version: candidate.authorization_version,
                membership_epoch: candidate.membership_epoch,
                cloud_access_epoch: candidate.cloud_access_epoch,
                placement: candidate
                    .placement
                    .map(|profile| cloud_id(profile.as_uuid())),
                placement_revision: candidate.placement_revision,
            },
        };
        if self.cancel.cancelled() {
            return Ok(None);
        }
        let permit = self.permit_v2(
            authority,
            &token,
            CloudPermitScopeV2::Workspace { descriptor },
        )?;
        if hosted {
            // Bootstrap for a Workspace this device does not hold: the
            // placement comes from the root unit joined earlier in the pass,
            // never from this descriptor. A refusal drops the permit before
            // it is installed anywhere.
            self.host
                .ensure_workspace_presence(&permit)
                .map_err(bootstrap_failure)?;
        }
        let content = Scoped {
            credential: token.credential,
            expires_at_seconds: token.expires_at_seconds,
            route: token.route,
            permit,
            issued_under: self.receipt_generation(),
        };
        let personal = self.admit_personal(authority, workspace);
        Ok(Some(Admission {
            candidate,
            content,
            personal,
            legacy_profile_units: None,
        }))
    }

    /// One Profile's Identity/Private admission (§4.4): the Profile must
    /// exist here — this is the one place `ensure_profile_presence` is
    /// still a prerequisite — and the service's `profile_units` token for
    /// it is the eligibility check on its side.
    fn admit_profile_units(
        &mut self,
        authority: &EnrollmentAuthorization,
        profile: ProfileId,
    ) -> Result<Scoped, Failure> {
        self.host
            .ensure_profile_presence(profile, authority.owner.account)
            .map_err(bootstrap_failure)?;
        let token = self.authorized(|api, authority| {
            api.issue_token_v2(
                authority,
                CloudSyncScopeV2::ProfileUnits {
                    profile_id: profile,
                },
            )
        })?;
        let Some(dataset @ CloudReplicaPrincipalV1::Profile { profile_id, .. }) = token.dataset
        else {
            return Err(Failure::DatasetMismatch);
        };
        if profile_id != cloud_id(profile.as_uuid()) {
            return Err(Failure::DatasetMismatch);
        }
        // The service picks the Workspace this credential is issued under
        // (one placed under the Profile) and says so only in the credential;
        // the worker scopes the session to it.
        let authority_workspace = token
            .credential
            .unverified_workspace_id()
            .ok_or(Failure::DatasetMismatch)?;
        let permit = self.permit_v2(
            authority,
            &token,
            CloudPermitScopeV2::ProfileUnits {
                dataset,
                allowed_profile_units: vec![
                    ProfileUnitRef::Identity { profile },
                    ProfileUnitRef::Private { profile },
                ],
                authority_workspace_id: Some(cloud_id(authority_workspace)),
            },
        )?;
        Ok(Scoped {
            credential: token.credential,
            expires_at_seconds: token.expires_at_seconds,
            route: token.route,
            permit,
            issued_under: self.receipt_generation(),
        })
    }

    /// §4.4 at this pass's revision: a Profile's Identity/Private units are
    /// eligible iff the service's committed projection finds it eligible
    /// **and** at least one Workspace this pass admitted (the device's own
    /// policy included) is currently placed under it. The set is recomputed
    /// from scratch every pass; an admission that is no longer eligible, was
    /// issued under an older authorization generation, has gone stale or
    /// errored is dropped here and fenced with the pass's stale permits.
    fn reconcile_profile_units(
        &mut self,
        authority: &EnrollmentAuthorization,
        admitted: &[Admission],
        clock: &dyn Clock,
    ) {
        let now_seconds = clock.now_millis().unwrap_or(i64::MAX) / 1000;
        let generation = self.receipt_generation();
        let placed: BTreeSet<ProfileId> = admitted
            .iter()
            .filter_map(|admission| match &admission.content.permit.scope {
                CloudPermitScopeV2::Workspace { descriptor } => descriptor
                    .placement
                    .map(|profile| ProfileId::from_uuid(profile.as_uuid())),
                _ => None,
            })
            .collect();
        let eligible: BTreeSet<ProfileId> = match self.contract {
            Some(CloudContract::V2) => self
                .receipt
                .eligible
                .intersection(&placed)
                .copied()
                .collect(),
            _ => admitted
                .iter()
                .filter_map(|admission| admission.candidate.legacy_profile)
                .collect(),
        };
        let held = std::mem::take(&mut self.profile_units);
        for (profile, scoped) in held {
            let keep = eligible.contains(&profile)
                && scoped.issued_under == generation
                && scoped.fresh(now_seconds)
                && self.healthy(scoped.permit.permit_id);
            if keep {
                self.profile_units.insert(profile, scoped);
            } else {
                tracing::info!(
                    target: "koru_cloud",
                    profile = ?profile,
                    eligible = eligible.contains(&profile),
                    "cloud pass: Profile units admission closed"
                );
            }
        }
        for profile in eligible {
            if self.profile_units.contains_key(&profile) {
                continue;
            }
            if self.cancel.cancelled() {
                return;
            }
            let admission = match self.contract {
                Some(CloudContract::V2) => self.admit_profile_units(authority, profile),
                // The v1 pair carried the Profile dataset with each
                // Workspace; the first admitted Workspace under the Profile
                // lends its admission, and the others open no second one.
                _ => admitted
                    .iter()
                    .find(|admission| admission.candidate.legacy_profile == Some(profile))
                    .and_then(|admission| admission.legacy_profile_units.clone())
                    .ok_or(Failure::DatasetMismatch),
            };
            match admission {
                Ok(scoped) => {
                    self.profile_units.insert(profile, scoped);
                }
                Err(error) => self.defer_profile(profile, error),
            }
        }
    }
}

mod catalog;
mod legacy;
mod parallel;
mod sessions;

/// The status record a freshly installed permit starts from.
fn record_for(permit: &CloudPermitV2, account: KoruIdentityId) -> StatusRecord {
    StatusRecord {
        permit_id: permit.permit_id,
        profile_id: permit
            .placement_profile()
            .map(|profile| ProfileId::from_uuid(profile.as_uuid())),
        account_id: account,
        account_generation: permit.account_generation,
        dataset: permit
            .dataset()
            .expect("a host-installed permit has a dataset session"),
        stage: Stage::Connecting,
        error_code: None,
        pending_consent_ids: Vec::new(),
        bytes_held: 0,
        queued_jobs: 0,
    }
}

/// What the hosted side said about the Workspace's content, as the v2
/// receipt carries it: being listed is the account's live membership (the
/// receipt's account was checked against the authority at enumeration), and
/// the catalog lifecycle says whether the content still exists at all.
fn hosted_facts(
    account: KoruIdentityId,
    candidate: &Candidate,
    logical_replica_id: CloudId,
) -> HostedContentFacts {
    HostedContentFacts {
        workspace_id: candidate.workspace_id,
        state: match candidate.lifecycle {
            SubjectLifecycle::Live | SubjectLifecycle::Deleted => WorkspaceCloudState::Enabled,
            SubjectLifecycle::Purged => WorkspaceCloudState::Absent,
        },
        incarnation_id: candidate.incarnation,
        logical_replica_id: logical_replica_id.as_uuid(),
        membership: Some(MembershipFact {
            account_id: account,
            membership_epoch: u64::try_from(candidate.membership_epoch).unwrap_or(0),
            active: true,
        }),
        authorization_version: u64::try_from(candidate.authorization_version).unwrap_or(0),
        cloud_access_epoch: u64::try_from(candidate.cloud_access_epoch).unwrap_or(0),
    }
}

/// The hosted v2 descriptor must name the candidate's content identity and
/// the issued dataset exactly; placement is not identity and is taken from
/// the fresher of the two without comparison.
fn correlate_v2(
    candidate: &Candidate,
    descriptor: &CloudWorkspaceDescriptorV2,
    dataset: CloudReplicaPrincipalV1,
) -> Result<(), Failure> {
    if descriptor.workspace_id != candidate.workspace_id
        || descriptor.workspace_incarnation_id != candidate.incarnation
        || descriptor.lineage_id != candidate.incarnation
    {
        return Err(Failure::DatasetMismatch);
    }
    let CloudReplicaPrincipalV1::Workspace {
        workspace_id,
        incarnation_id,
        logical_replica_id,
    } = dataset
    else {
        return Err(Failure::DatasetMismatch);
    };
    if workspace_id != cloud_id(descriptor.workspace_id.as_uuid())
        || incarnation_id != cloud_id(descriptor.workspace_incarnation_id)
        || logical_replica_id != cloud_id(descriptor.logical_replica_id)
    {
        return Err(Failure::DatasetMismatch);
    }
    Ok(())
}

/// The wire descriptor in the control protocol's spelling.
fn control_descriptor_v2(descriptor: &CloudWorkspaceDescriptorV2) -> ControlDescriptorV2 {
    ControlDescriptorV2 {
        version: koru_sync_control::cloud::CLOUD_WORKSPACE_DESCRIPTOR_VERSION_V2,
        workspace_id: cloud_id(descriptor.workspace_id.as_uuid()),
        workspace_incarnation_id: cloud_id(descriptor.workspace_incarnation_id),
        logical_replica_id: cloud_id(descriptor.logical_replica_id),
        lineage_id: cloud_id(descriptor.lineage_id),
        manifest_document_id: cloud_id(descriptor.manifest_document_id),
        authorization_version: descriptor.authorization_version,
        membership_epoch: descriptor.membership_epoch,
        cloud_access_epoch: descriptor.cloud_access_epoch,
        placement: descriptor
            .placement
            .placement
            .map(|profile| cloud_id(profile.as_uuid())),
        placement_revision: descriptor.placement.placement_revision,
    }
}

/// A v1 dataset route as the v2 shape every session is opened from.
pub(super) fn route_of(route: &DatasetRouteResultV1) -> CloudRouteResultV2 {
    match route {
        DatasetRouteResultV1::Available { route, .. } => CloudRouteResultV2::Available {
            route: route.clone(),
        },
        DatasetRouteResultV1::Unavailable {
            code,
            retry_after_seconds,
            ..
        } => CloudRouteResultV2::Unavailable {
            code: *code,
            retry_after_seconds: *retry_after_seconds,
        },
    }
}

/// `CloudUnitRefV2` as the Store's receipt key.
pub(super) fn join_unit(unit: CloudUnitRefV2) -> JoinUnit {
    match unit {
        CloudUnitRefV2::ProfileCatalog { subject_id } => JoinUnit::ProfileCatalog {
            profile: subject_id,
        },
        CloudUnitRefV2::WorkspaceCatalog { subject_id } => JoinUnit::WorkspaceCatalog {
            workspace: subject_id,
        },
        CloudUnitRefV2::PersonalV2 { subject_id } => JoinUnit::PersonalV2 {
            workspace: subject_id,
        },
    }
}

pub(super) fn unit_ref(unit: JoinUnit) -> CloudUnitRefV2 {
    match unit {
        JoinUnit::ProfileCatalog { profile } => CloudUnitRefV2::ProfileCatalog {
            subject_id: profile,
        },
        JoinUnit::WorkspaceCatalog { workspace } => CloudUnitRefV2::WorkspaceCatalog {
            subject_id: workspace,
        },
        JoinUnit::PersonalV2 { workspace } => CloudUnitRefV2::PersonalV2 {
            subject_id: workspace,
        },
    }
}

/// The first refusal of a pass is the one the host sees; a later one does not
/// relabel it, except that a terminal refusal always wins over a retryable
/// one so the pass does not schedule a retry it cannot make progress on.
pub(super) fn merge(previous: Option<Failure>, next: Failure) -> Failure {
    match previous {
        Some(previous) if previous.automatic() && !next.automatic() => next,
        Some(previous) => previous,
        None => next,
    }
}

/// A per-unit refusal is terminal **for its unit**, never for the account
/// (QUESTIONS.md Q12 (a), (b)). Off its own route it carries no unit, so it
/// is read as the ordinary unavailable answer rather than as a reason to
/// stop retrying everything.
pub(super) fn not_per_unit(failure: Failure) -> Failure {
    match failure {
        Failure::UnitFormatTooNew | Failure::UnitRefused(_) => Failure::Unavailable,
        other => other,
    }
}

/// A Store refusal is a bootstrap failure and is retried, but a revocation
/// that reached the host through it stays terminal: retrying cannot fix it.
/// A pending placement keeps its own word, so the pass knows to retry after
/// the next root join rather than report a broken bootstrap.
pub(super) fn bootstrap_failure(error: Failure) -> Failure {
    match error {
        Failure::SessionExpired | Failure::AccessRevoked | Failure::PlacementPending => error,
        _ => Failure::Bootstrap,
    }
}

pub(super) fn stage_for(failure: &Failure) -> Stage {
    match failure {
        Failure::Offline => Stage::Offline,
        Failure::SessionExpired | Failure::AccessRevoked => Stage::AwaitingAuthority,
        _ => Stage::Unavailable,
    }
}
