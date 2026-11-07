// SPDX-License-Identifier: MPL-2.0

//! One bounded pass over the account's Cloud catalog.
//!
//! The order inside a pass is fixed: discovery, selection, identity,
//! Profile-first bootstrap, permits, credential, sessions. Every step is a
//! bounded call on an injected port,
//! and the coordinator holds no lock while one runs — it owns its state
//! exclusively on the worker thread and publishes a whole snapshot after.

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use koru_account_wire::cloud_sync::{DatasetRouteResultV1, SyncTokenResponseV1};

use super::*;
use crate::ports::{Clock, Entropy};

/// What one pair reached before its datasets were opened.
#[derive(Clone)]
struct Admission {
    candidate: CloudWorkspaceCandidateV1,
    token: SyncTokenResponseV1,
    workspace_permit: CloudPermitV1,
    profile_permit: CloudPermitV1,
}

/// The identity a Workspace permit is built from, and where it came from.
enum Identity {
    /// HOST-CONTROL §3.1: this device originated the Workspace, so the
    /// manifest id is the durable local one and the rest of the descriptor is
    /// formed from the current credential context.
    Local(LocalBootstrap),
    /// The hosted §3.1 lookup answered a retained mapping.
    Hosted(CloudWorkspaceDescriptorV1),
}

pub(super) struct Coordinator<A: Api, H: Host> {
    api: A,
    host: Arc<H>,
    entropy: Arc<dyn Entropy>,
    cancel: Cancel,
    authority: Option<EnrollmentAuthorization>,
    owner_scope: Option<u64>,
    /// In memory only. A replaced authorization increments it; every older
    /// permit is fenced and dropped with it.
    account_generation: u64,
    /// Strictly increasing within this coordinator session. A restart
    /// invalidates the whole sequence space.
    sequence: u64,
    live: Vec<CloudPermitV1>,
    admissions: BTreeMap<WorkspaceId, Admission>,
    records: BTreeMap<Uuid, StatusRecord>,
    deferred: Vec<Deferred>,
    deferred_failure: Option<Failure>,
    discovery: Option<DiscoveryReceipt>,
    discovery_counts: BTreeMap<ProfileId, DiscoveryCounts>,
    discovery_exhausted: bool,
    carried: VecDeque<CloudWorkspaceCandidateV1>,
    /// The verification material, bound to the generation it was read under.
    jwks: Option<(u64, JwksResponseV1)>,
    pending: bool,
    retry_at: Option<Instant>,
    debounce_at: Option<Instant>,
    failures: u32,
    failure_code: Option<&'static str>,
    retry_at_millis: Option<i64>,
    revision: u64,
    stage: Stage,
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
            records: BTreeMap::new(),
            deferred: Vec::new(),
            deferred_failure: None,
            discovery: None,
            discovery_counts: BTreeMap::new(),
            discovery_exhausted: false,
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

    fn session_access(&self, authority: &EnrollmentAuthorization) -> SessionAccess {
        SessionAccess {
            authority: authority.clone(),
            desired: self.desired.clone(),
            cancel: self.cancel.clone(),
            foreground: self.foreground.clone(),
        }
    }

    fn current_authority(&self) -> Result<EnrollmentAuthorization, Failure> {
        self.authority
            .as_ref()
            .and_then(|authority| self.session_access(authority).current())
            .ok_or(Failure::SessionExpired)
    }

    fn publish_progress(&self) {
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

    fn replace_generation(&mut self) {
        let stale: Vec<Uuid> = self.live.iter().map(|permit| permit.permit_id).collect();
        self.live.clear();
        self.admissions.clear();
        self.records.clear();
        self.deferred.clear();
        self.deferred_failure = None;
        self.discovery = None;
        self.discovery_counts.clear();
        self.discovery_exhausted = false;
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
        }
    }

    fn mint(&self) -> Uuid {
        let mut bytes = [0_u8; 16];
        self.entropy.fill_bytes(&mut bytes);
        uuid::Builder::from_random_bytes(bytes).into_uuid()
    }

    // ----- one pass -------------------------------------------------------

    fn run_pass(&mut self, clock: &dyn Clock) {
        let Some(authority) = self.authority.clone() else {
            return;
        };
        self.revision = self.revision.saturating_add(1);
        self.deferred.clear();
        self.deferred_failure = None;
        self.discovery_counts.clear();
        self.discovery_exhausted = false;
        for candidate in &self.carried {
            self.discovery_counts
                .entry(candidate.profile_id)
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

        if self.carried.is_empty()
            && let Err(error) = self.enumerate()
        {
            // Without a catalog there is nothing to admit this pass. The
            // live context is left alone; a stale permit is fenced only by an
            // authority change or by the next successful enumeration.
            self.stage = stage_for(&error);
            self.settle(clock, Some(error));
            return;
        }
        if let Err(error) = self.refresh_verification() {
            self.stage = stage_for(&error);
            self.settle(clock, Some(error));
            return;
        }

        let mut cohort = VecDeque::with_capacity(self.carried.len());
        while cohort.len() < MAX_CARRIED_CANDIDATES {
            match self.carried.pop_front() {
                Some(candidate) => cohort.push_back(candidate),
                None => break,
            }
        }

        let mut admitted = Vec::with_capacity(cohort.len());
        while let Some(candidate) = cohort.pop_front() {
            if self.cancel.cancelled() {
                self.carried.push_front(candidate);
                self.carried.extend(cohort);
                break;
            }
            let pair = (candidate.profile_id, candidate.workspace_id);
            let cached = self
                .admissions
                .get(&candidate.workspace_id)
                .filter(|previous| {
                    previous.candidate == candidate
                        && [
                            previous.workspace_permit.permit_id,
                            previous.profile_permit.permit_id,
                        ]
                        .iter()
                        .all(|id| {
                            self.records.get(id).is_some_and(|record| {
                                record.error_code.is_none()
                                    && matches!(
                                        record.stage,
                                        Stage::Settled
                                            | Stage::CheckingCoverage
                                            | Stage::Reconciling
                                    )
                            })
                        })
                        && previous
                            .token
                            .expires_at_seconds()
                            .saturating_sub(clock.now_millis().unwrap_or(i64::MAX) / 1000)
                            > SESSION_RENEW_BEFORE_SECONDS
                })
                .cloned();
            let admission = match cached {
                Some(admission) => Ok(Some(admission)),
                None => self.admit(&authority, candidate),
            };
            match admission {
                Ok(Some(admission)) => admitted.push(admission),
                Ok(None) => {}
                Err(error) => {
                    self.defer(pair, error.clone());
                    failure = Some(merge(failure, error));
                }
            }
        }

        // The standing permit ceiling is a hard limit, not a truncation of an
        // acknowledged context: a pair over the budget is deferred whole.
        while admitted.len() * 2 > MAX_LIVE_PERMITS {
            if let Some(dropped) = admitted.pop() {
                self.defer(
                    (dropped.candidate.profile_id, dropped.candidate.workspace_id),
                    Failure::Unavailable,
                );
            }
        }

        let retained: Vec<Uuid> = admitted
            .iter()
            .flat_map(|a| [a.workspace_permit.permit_id, a.profile_permit.permit_id])
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
            .retain(|_, a| retained.contains(&a.workspace_permit.permit_id));
        self.records.retain(|id, _| retained.contains(id));

        let permits: Vec<CloudPermitV1> = admitted
            .iter()
            .flat_map(|admission| {
                [
                    admission.workspace_permit.clone(),
                    admission.profile_permit.clone(),
                ]
            })
            .collect();
        for admission in &admitted {
            self.discovery_counts
                .entry(admission.candidate.profile_id)
                .or_default()
                .admitted += 1;
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
                .or_insert(StatusRecord {
                    permit_id: permit.permit_id,
                    profile_id: permit.profile_id,
                    account_id: permit.account_id,
                    account_generation: permit.account_generation,
                    dataset: permit.dataset,
                    stage: Stage::Connecting,
                    error_code: None,
                    pending_consent_ids: Vec::new(),
                    bytes_held: 0,
                    queued_jobs: 0,
                });
        }
        self.live = permits;

        let hinted = self.run_sessions(admitted, clock, &mut failure);

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
        let mut after = None;
        let mut candidates = VecDeque::new();
        loop {
            if self.cancel.cancelled() {
                break;
            }
            let page = self.authorized(|api, authority| api.discover(authority, after.clone()))?;
            for candidate in page.workspaces {
                self.discovery_counts
                    .entry(candidate.profile_id)
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
            match page.next {
                Some(cursor) => after = Some(cursor),
                None => {
                    self.discovery_exhausted = true;
                    break;
                }
            }
        }
        // Round robin before taking a bounded cohort: a large Profile cannot
        // occupy all sixteen slots ahead of every other Profile.
        let mut profiles = BTreeMap::<ProfileId, VecDeque<_>>::new();
        for candidate in candidates {
            profiles
                .entry(candidate.profile_id)
                .or_default()
                .push_back(candidate);
        }
        self.carried.clear();
        while profiles.values().any(|queue| !queue.is_empty()) {
            for queue in profiles.values_mut() {
                if let Some(candidate) = queue.pop_front() {
                    self.carried.push_back(candidate);
                }
            }
        }
        Ok(())
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

    fn defer(&mut self, pair: (ProfileId, WorkspaceId), error: Failure) {
        let code = error.code();
        self.deferred_failure = Some(merge(self.deferred_failure.take(), error));
        let counts = self.discovery_counts.entry(pair.0).or_default();
        counts.deferred += 1;
        counts.deferred_code.get_or_insert(code);
        // Which workspace, and why, or the only trace of a deferred dataset
        // is a counter. Ids and a stable code; nothing a person wrote.
        tracing::info!(
            target: "koru_cloud",
            workspace = ?pair.1,
            code,
            "cloud pass: deferred a workspace"
        );
        self.deferred.push(Deferred {
            profile_id: pair.0,
            workspace_id: pair.1,
            code,
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
                .entry(candidate.profile_id)
                .or_default()
                .carried += 1;
        }
        self.discovery = self.authority.as_ref().map(|authority| DiscoveryReceipt {
            account_id: authority.owner.account,
            pass: self.revision,
            stale: false,
            exhausted: self.discovery_exhausted,
            profiles: self.discovery_counts.clone(),
            failure_code: self.failure_code,
            retry_at_millis: self.retry_at_millis,
            automatic_retry,
        });
    }

    /// Steps 3 to 7 for one discovery pair. `Ok(None)` means the pass was
    /// cancelled part-way; a refusal is an `Err` the caller records as a
    /// deferred pair. Nothing is invented for either.
    fn admit(
        &mut self,
        authority: &EnrollmentAuthorization,
        candidate: CloudWorkspaceCandidateV1,
    ) -> Result<Option<Admission>, Failure> {
        let profile = candidate.profile_id;
        let workspace = candidate.workspace_id;

        // Idempotent, once per pass. A 403 denies this pair for the pass.
        self.authorized(|api, authority| api.select(authority, profile, workspace))?;
        if self.cancel.cancelled() {
            return Ok(None);
        }

        // The credential first: the originating-device read names the exact
        // issued Workspace dataset (HOST-CONTROL §3.1 `read_cloud_bootstrap`),
        // so the host answers for that replica and not for a Workspace id
        // alone. A token wasted on a refused bootstrap is cheaper than a
        // bootstrap read that had to guess a replica.
        let mut token =
            self.authorized(|api, authority| api.issue_token(authority, workspace, profile))?;
        let identity = match self.host.local_workspace(
            authority.owner.account,
            profile,
            token.workspace().dataset(),
        )? {
            Some(local) => {
                if local.profile_id != profile || local.workspace_id != workspace {
                    return Err(Failure::DatasetMismatch);
                }
                Identity::Local(local)
            }
            // A 503 is "not ready": skip the Workspace this pass and retry
            // with backoff rather than inventing an identity for it.
            None => Identity::Hosted(
                self.authorized(|api, authority| api.describe(authority, workspace, profile))?,
            ),
        };

        // Profile-first bootstrap, before any permit for a Workspace this
        // device does not hold (HOST-CONTROL §3.1 bootstrap order, R1).
        if let Identity::Hosted(descriptor) = &identity {
            self.host
                .ensure_profile_presence(profile, authority.owner.account)
                .map_err(bootstrap_failure)?;
            self.host
                .ensure_workspace_presence(descriptor)
                .map_err(bootstrap_failure)?;
        }
        if self.cancel.cancelled() {
            return Ok(None);
        }

        let mut descriptor = self.descriptor(&candidate, &identity, &token)?;
        if let Err(Failure::DatasetMismatch) = self.correlate(&candidate, &descriptor, &token) {
            // One bounded re-read and reissue. Selection can invalidate
            // epochs; values from two authority generations are never mixed.
            token =
                self.authorized(|api, authority| api.issue_token(authority, workspace, profile))?;
            let identity = match self.host.local_workspace(
                authority.owner.account,
                profile,
                token.workspace().dataset(),
            )? {
                Some(local) => Identity::Local(local),
                None => Identity::Hosted(
                    self.authorized(|api, authority| api.describe(authority, workspace, profile))?,
                ),
            };
            descriptor = self.descriptor(&candidate, &identity, &token)?;
            self.correlate(&candidate, &descriptor, &token)?;
        }

        let expires_at_seconds = token.expires_at_seconds();
        let workspace_permit = CloudPermitV1 {
            version: 1,
            permit_id: self.mint(),
            dataset: token.workspace().dataset(),
            account_id: authority.owner.account,
            profile_id: profile,
            authority_workspace_id: cloud_id(workspace.as_uuid()),
            authority_workspace_incarnation_id: cloud_id(candidate.workspace_incarnation_id),
            account_generation: self.account_generation,
            expires_at_seconds,
            allowed_profile_units: Vec::new(),
            workspace_descriptor: Some(descriptor),
        };
        let profile_permit = CloudPermitV1 {
            version: 1,
            permit_id: self.mint(),
            dataset: token.profile().dataset(),
            account_id: authority.owner.account,
            profile_id: profile,
            authority_workspace_id: cloud_id(workspace.as_uuid()),
            authority_workspace_incarnation_id: cloud_id(candidate.workspace_incarnation_id),
            account_generation: self.account_generation,
            expires_at_seconds,
            // The three units of the exact authorizing Workspace. No union of
            // Personal scopes from unrelated Workspaces; the host narrows
            // further by the grant it can prove.
            allowed_profile_units: vec![
                ProfileUnitRef::Identity { profile },
                ProfileUnitRef::Private { profile },
                ProfileUnitRef::Personal {
                    owner: profile,
                    workspace,
                },
            ],
            workspace_descriptor: None,
        };
        Ok(Some(Admission {
            candidate,
            token,
            workspace_permit,
            profile_permit,
        }))
    }

    /// The identity-only descriptor the Workspace permit carries. For the
    /// originating device it is the durable local manifest id plus the
    /// current candidate and credential context (HOST-CONTROL §3.1); nothing
    /// is invented when neither source has it.
    fn descriptor(
        &self,
        candidate: &CloudWorkspaceCandidateV1,
        identity: &Identity,
        token: &SyncTokenResponseV1,
    ) -> Result<CloudWorkspaceDescriptorV1, Failure> {
        match identity {
            Identity::Hosted(descriptor) => Ok(*descriptor),
            Identity::Local(local) => {
                let CloudReplicaPrincipalV1::Workspace {
                    workspace_id,
                    incarnation_id,
                    logical_replica_id,
                } = token.workspace().dataset()
                else {
                    return Err(Failure::DatasetMismatch);
                };
                if workspace_id != cloud_id(candidate.workspace_id.as_uuid())
                    || incarnation_id != cloud_id(candidate.workspace_incarnation_id)
                {
                    return Err(Failure::DatasetMismatch);
                }
                Ok(CloudWorkspaceDescriptorV1 {
                    version: koru_account_wire::Version1,
                    profile_id: local.profile_id,
                    workspace_id: local.workspace_id,
                    workspace_incarnation_id: candidate.workspace_incarnation_id,
                    logical_replica_id: Uuid::from_bytes(*logical_replica_id.as_bytes()),
                    lineage_id: candidate.workspace_incarnation_id,
                    manifest_document_id: local.manifest_document_id,
                    authorization_version: candidate.authorization_version,
                    membership_epoch: candidate.membership_epoch,
                    cloud_access_epoch: candidate.cloud_access_epoch,
                })
            }
        }
    }

    /// The issued dataset principals must be the ones the descriptor and the
    /// candidate already name. A mismatch is refused, never reconciled by
    /// taking one field from each side.
    fn correlate(
        &self,
        candidate: &CloudWorkspaceCandidateV1,
        descriptor: &CloudWorkspaceDescriptorV1,
        token: &SyncTokenResponseV1,
    ) -> Result<(), Failure> {
        if descriptor.workspace_id != candidate.workspace_id
            || descriptor.profile_id != candidate.profile_id
            || descriptor.workspace_incarnation_id != candidate.workspace_incarnation_id
            || descriptor.lineage_id != candidate.workspace_incarnation_id
        {
            return Err(Failure::DatasetMismatch);
        }
        let CloudReplicaPrincipalV1::Workspace {
            workspace_id,
            incarnation_id,
            logical_replica_id,
        } = token.workspace().dataset()
        else {
            return Err(Failure::DatasetMismatch);
        };
        if workspace_id != cloud_id(descriptor.workspace_id.as_uuid())
            || incarnation_id != cloud_id(descriptor.workspace_incarnation_id)
            || logical_replica_id != cloud_id(descriptor.logical_replica_id)
        {
            return Err(Failure::DatasetMismatch);
        }
        let CloudReplicaPrincipalV1::Profile { profile_id, .. } = token.profile().dataset() else {
            return Err(Failure::DatasetMismatch);
        };
        if profile_id != cloud_id(descriptor.profile_id.as_uuid()) {
            return Err(Failure::DatasetMismatch);
        }
        Ok(())
    }
}

mod parallel;
mod sessions;

/// The first refusal of a pass is the one the host sees; a later one does not
/// relabel it, except that a terminal refusal always wins over a retryable
/// one so the pass does not schedule a retry it cannot make progress on.
fn merge(previous: Option<Failure>, next: Failure) -> Failure {
    match previous {
        Some(previous) if previous.automatic() && !next.automatic() => next,
        Some(previous) => previous,
        None => next,
    }
}

/// A Store refusal is a bootstrap failure and is retried, but a revocation
/// that reached the host through it stays terminal: retrying cannot fix it.
fn bootstrap_failure(error: Failure) -> Failure {
    match error {
        Failure::SessionExpired | Failure::AccessRevoked => error,
        _ => Failure::Bootstrap,
    }
}

fn stage_for(failure: &Failure) -> Stage {
    match failure {
        Failure::Offline => Stage::Offline,
        Failure::SessionExpired | Failure::AccessRevoked => Stage::AwaitingAuthority,
        _ => Stage::Unavailable,
    }
}
