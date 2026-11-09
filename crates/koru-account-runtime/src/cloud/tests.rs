// SPDX-License-Identifier: MPL-2.0

//! The whole coordinator, driven through its real worker with in-memory
//! ports. Nothing here touches disk, the network, a Store or telemetry: the
//! `Api` fake answers the hosted reads — the v2 catalog contract and the v1
//! pair path of the compatibility window — from a table, and the `Host` fake
//! records the platform effects in the order they were asked for.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use koru_account_core::root_catalog::{SubjectLifecycle, WorkspacePlacement};
use koru_account_wire::cloud_sync::{
    DatasetRouteResultV1, SyncCredential, SyncTokenResponseV1, SyncUnavailableCode,
};
use koru_account_wire::{
    AccessToken, CloudCoverageV2, CloudDiscoveryScopeV2, CloudMembershipV2, CloudProfileResultV2,
    CloudRootUnitStateV2, CloudRouteResultV2, CloudWorkspaceCandidateKindV1,
    CloudWorkspaceCandidateV2, JwkV1, JwksResponseV1, Version1, Version2,
};
use koru_device::ReplicationEndpointId;
use koru_store_core::profile_units::ProfileUnitRef;

use super::*;
use crate::identity::AccountOwner;
use crate::ports::{Clock, Entropy};

mod discovery;

// ----- fixtures -----------------------------------------------------------

struct PauseClock {
    origin: Instant,
    elapsed: AtomicU64,
}
impl PauseClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
            elapsed: AtomicU64::new(0),
        })
    }
    fn advance(&self, millis: u64) {
        self.elapsed.fetch_add(millis, Ordering::AcqRel);
    }
}
impl Clock for PauseClock {
    fn now_millis(&self) -> Option<i64> {
        Some(1_000 + self.elapsed.load(Ordering::Acquire) as i64)
    }
    fn monotonic(&self) -> Instant {
        self.origin + Duration::from_millis(self.elapsed.load(Ordering::Acquire))
    }
}

/// Deterministic permit ids: the worker mints v4 values from these bytes, so a
/// failing test names the same permit every run.
#[derive(Default)]
struct Steps(AtomicU64);
impl Entropy for Steps {
    fn fill_bytes(&self, bytes: &mut [u8]) {
        let step = self.0.fetch_add(1, Ordering::AcqRel) + 1;
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = step as u8 ^ index as u8;
        }
    }
}

fn account() -> KoruIdentityId {
    "30000000-0000-4000-8000-000000000001".parse().unwrap()
}

fn other_account() -> KoruIdentityId {
    "30000000-0000-4000-8000-000000000002".parse().unwrap()
}

fn scope() -> CatalogScopeId {
    "90000000-0000-4000-8000-000000000001".parse().unwrap()
}

fn authority(session: &str) -> EnrollmentAuthorization {
    EnrollmentAuthorization {
        access: AccessToken(format!("access-{session}")),
        owner: AccountOwner {
            account: account(),
            auth_identity: "40000000-0000-4000-8000-000000000001".parse().unwrap(),
        },
        namespace: "n".repeat(64),
        record_version: 3,
        session_id: format!("5000000{session}-0000-4000-8000-000000000001"),
    }
}

fn profile(index: u8) -> ProfileId {
    ProfileId::parse(&format!("10000000-0000-4000-8000-0000000000{index:02x}")).unwrap()
}
fn workspace(index: u8) -> WorkspaceId {
    WorkspaceId::parse(&format!("20000000-0000-4000-8000-0000000000{index:02x}")).unwrap()
}
fn incarnation(index: u8) -> Uuid {
    Uuid::parse_str(&format!("60000000-0000-4000-8000-0000000000{index:02x}")).unwrap()
}
fn replica(index: u8) -> Uuid {
    Uuid::parse_str(&format!("70000000-0000-4000-8000-0000000000{index:02x}")).unwrap()
}
fn manifest(index: u8) -> Uuid {
    Uuid::parse_str(&format!("80000000-0000-4000-8000-0000000000{index:02x}")).unwrap()
}

/// A v1 candidate, for the compatibility window.
fn candidate(index: u8) -> CloudWorkspaceCandidateV1 {
    CloudWorkspaceCandidateV1 {
        kind: CloudWorkspaceCandidateKindV1::Cloud,
        profile_id: profile(index),
        workspace_id: workspace(index),
        workspace_incarnation_id: incarnation(index),
        authorization_version: 4,
        membership_epoch: 5,
        cloud_access_epoch: 6,
    }
}

/// A v2 candidate: Workspace `index`, placed under Profile `placement`
/// (`None` is explicitly unassigned).
fn candidate_v2(index: u8, placement: Option<u8>) -> CloudWorkspaceCandidateV2 {
    CloudWorkspaceCandidateV2 {
        workspace_id: workspace(index),
        workspace_incarnation_id: incarnation(index),
        lifecycle: SubjectLifecycle::Live,
        membership: CloudMembershipV2 {
            authorization_version: 4,
            membership_epoch: 5,
            cloud_access_epoch: 6,
            documents_write: true,
        },
        placement: WorkspacePlacement {
            placement: placement.map(profile),
            placement_revision: 1,
        },
        projection_watermark: 1,
    }
}

fn profile_result(index: u8, eligible: bool, lifecycle: SubjectLifecycle) -> CloudProfileResultV2 {
    CloudProfileResultV2 {
        profile_id: profile(index),
        lifecycle,
        profile_units_eligible: eligible,
        projection_watermark: 1,
    }
}

fn live(index: u8) -> CloudProfileResultV2 {
    profile_result(index, true, SubjectLifecycle::Live)
}

fn receipt(
    results: Vec<CloudWorkspaceCandidateV2>,
    profiles: Vec<CloudProfileResultV2>,
    coverage: CloudCoverageV2,
    next: Option<&CloudWorkspaceCandidateV2>,
) -> CloudDiscoveryReceiptV2 {
    CloudDiscoveryReceiptV2 {
        version: Version2,
        account_id: account(),
        catalog_scope_id: scope(),
        authorization_generation: 1,
        pass: 1,
        scope: CloudDiscoveryScopeV2::Workspaces,
        coverage,
        profiles,
        results,
        next: next.map(|candidate| CloudDiscoveryCursorV2 {
            workspace_id: candidate.workspace_id,
        }),
    }
}

/// The v1 hosted descriptor, for the compatibility window.
fn descriptor(index: u8) -> CloudWorkspaceDescriptorV1 {
    CloudWorkspaceDescriptorV1 {
        version: Version1,
        profile_id: profile(index),
        workspace_id: workspace(index),
        workspace_incarnation_id: incarnation(index),
        logical_replica_id: replica(index),
        lineage_id: incarnation(index),
        manifest_document_id: manifest(index),
        authorization_version: 4,
        membership_epoch: 5,
        cloud_access_epoch: 6,
    }
}

/// The v2 hosted descriptor: content identity with the placement beside it.
fn descriptor_v2(index: u8, placement: Option<u8>) -> CloudWorkspaceDescriptorV2 {
    CloudWorkspaceDescriptorV2 {
        version: Version2,
        workspace_id: workspace(index),
        workspace_incarnation_id: incarnation(index),
        logical_replica_id: replica(index),
        lineage_id: incarnation(index),
        manifest_document_id: manifest(index),
        authorization_version: 4,
        membership_epoch: 5,
        cloud_access_epoch: 6,
        placement: WorkspacePlacement {
            placement: placement.map(profile),
            placement_revision: 1,
        },
    }
}

fn credential() -> SyncCredential {
    let head = URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA"}"#);
    let body = URL_SAFE_NO_PAD.encode(br#"{"sub":"test"}"#);
    let signature = URL_SAFE_NO_PAD.encode([9_u8; 64]);
    SyncCredential::try_from(format!("{head}.{body}.{signature}")).unwrap()
}

fn route() -> CloudSyncRouteV1 {
    let key = ed25519_dalek::SigningKey::from_bytes(&[7_u8; 32])
        .verifying_key()
        .to_bytes();
    CloudSyncRouteV1::new(
        ReplicationEndpointId::from_bytes(key),
        vec!["127.0.0.1:7777".into()],
        None,
    )
    .unwrap()
}

fn workspace_dataset(index: u8, logical: Uuid) -> CloudReplicaPrincipalV1 {
    CloudReplicaPrincipalV1::Workspace {
        workspace_id: cloud_id(workspace(index).as_uuid()),
        incarnation_id: cloud_id(incarnation(index)),
        logical_replica_id: cloud_id(logical),
    }
}

fn profile_dataset(index: u8) -> CloudReplicaPrincipalV1 {
    CloudReplicaPrincipalV1::Profile {
        profile_id: cloud_id(profile(index).as_uuid()),
        lineage_id: cloud_id(incarnation(index)),
        logical_replica_id: cloud_id(replica(index)),
    }
}

fn token_for_profile(
    index: u8,
    logical: Uuid,
    profile_available: bool,
    owner: u8,
) -> SyncTokenResponseV1 {
    let profile_result = if profile_available {
        DatasetRouteResultV1::Available {
            dataset: profile_dataset(owner),
            route: route(),
        }
    } else {
        DatasetRouteResultV1::Unavailable {
            dataset: profile_dataset(owner),
            code: SyncUnavailableCode::SyncUnavailable,
            retry_after_seconds: 30,
        }
    };
    SyncTokenResponseV1::new(
        credential(),
        4_000,
        DatasetRouteResultV1::Available {
            dataset: workspace_dataset(index, logical),
            route: route(),
        },
        profile_result,
    )
    .unwrap()
}

fn jwks() -> JwksResponseV1 {
    JwksResponseV1 {
        version: Version1,
        keys: Vec::<JwkV1>::new(),
        attestation_keys: Vec::new(),
        capability_keys: Vec::new(),
    }
}

fn unit_label(unit: CloudUnitRefV2) -> String {
    match unit {
        CloudUnitRefV2::ProfileCatalog { subject_id } => {
            format!("profile:{}", index_of_profile(subject_id))
        }
        CloudUnitRefV2::WorkspaceCatalog { subject_id } => {
            format!("workspace:{}", index_of(subject_id))
        }
        CloudUnitRefV2::PersonalV2 { subject_id } => format!("personal:{}", index_of(subject_id)),
    }
}

fn join_label(unit: JoinUnit) -> String {
    unit_label(super::coordinator::unit_ref(unit))
}

fn scope_label(scope: CloudSyncScopeV2) -> String {
    match scope {
        CloudSyncScopeV2::UserCatalog => "user_catalog".to_owned(),
        CloudSyncScopeV2::Workspace { workspace_id } => {
            format!("workspace:{}", index_of(workspace_id))
        }
        CloudSyncScopeV2::UserPersonal { workspace_id } => {
            format!("user_personal:{}", index_of(workspace_id))
        }
        CloudSyncScopeV2::ProfileUnits { profile_id } => {
            format!("profile_units:{}", index_of_profile(profile_id))
        }
    }
}

/// One shared table behind both fakes. The coordinator is the only writer of
/// its own state; everything here is an observation of what it asked for.
#[derive(Default)]
struct World {
    log: Vec<String>,
    /// Which contract the service speaks: `V2` answers the binding route,
    /// `V1` answers it with 404 and serves the pair routes.
    v1_service: bool,
    /// Answer the binding route over the *real* account client and the real
    /// hosted mapping, with this HTTP status and a body that is not Koru's
    /// error envelope — which is what the live v1 service answers. Set, no
    /// `Failure` is injected for the binding at all.
    hosted_binding_status: Option<u16>,
    /// The service refuses the binding: this device belongs to another
    /// account.
    binding_conflict: bool,
    /// What the host's Store holds as its binding, and whether the Store
    /// refuses the service's answer.
    host_binding: Option<CloudAccountBindingRef>,
    host_bind_refuses: bool,
    bound: Vec<CloudCatalogBindingV2>,
    /// v2 discovery pages and v1 pages.
    receipts: Vec<CloudDiscoveryReceiptV2>,
    pages: Vec<CloudWorkspacePageV1>,
    /// The service's catalog (unit → frontier) and what a pull answers.
    service_units: BTreeMap<CloudUnitRefV2, Vec<u8>>,
    service_frames: HashMap<CloudUnitRefV2, Vec<Vec<u8>>>,
    /// What a push is answered with, when not `Applied`.
    push_receipt: Option<CloudRootJoinReceiptV2>,
    pushed: Vec<CloudUnitRefV2>,
    /// The host's eligible inventory, frontiers and exports.
    local_units: Vec<CloudRootUnitState>,
    local_frontiers: HashMap<String, Vec<u8>>,
    exports: HashMap<String, CloudRootExport>,
    joined: Vec<String>,
    /// The token the issuer answers: expiry and authority generation.
    token_expiry: i64,
    authority_generation: u64,
    denied_scopes: HashSet<String>,
    denied: HashSet<WorkspaceId>,
    removed_profiles: HashSet<ProfileId>,
    placement_pending: HashSet<WorkspaceId>,
    not_ready: HashSet<WorkspaceId>,
    descriptors: HashMap<WorkspaceId, CloudWorkspaceDescriptorV1>,
    descriptors_v2: HashMap<WorkspaceId, CloudWorkspaceDescriptorV2>,
    /// The logical replica id the issuer answers with, so a test can make the
    /// credential disagree with the descriptor.
    issued_replica: HashMap<WorkspaceId, Uuid>,
    profile_route_available: bool,
    discover_failures: usize,
    discover_auth_failures: usize,
    discover_access_failures: usize,
    /// Request cancellation while the selected discovery page is in flight.
    cancel_discovery_page: Option<(usize, Cancel)>,
    local: HashMap<WorkspaceId, LocalBootstrap>,
    contexts: Vec<(u64, u64, Vec<CloudPermitV2>)>,
    fenced: Vec<Vec<Uuid>>,
    opened: Vec<CloudReplicaPrincipalV1>,
    /// Blob access tokens handed to the host: once per pass, then once
    /// before each dataset session.
    access_pushes: usize,
    remote_hints: usize,
    open_failures: usize,
    expired_opens: usize,
    blocked: HashMap<WorkspaceId, Arc<Mutex<mpsc::Receiver<()>>>>,
    live_permits: BTreeSet<Uuid>,
}

impl World {
    fn shared() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            profile_route_available: true,
            token_expiry: 4_000,
            authority_generation: 1,
            ..Self::default()
        }))
    }
    fn note(&mut self, entry: impl Into<String>) {
        self.log.push(entry.into());
    }
    fn permits(&self) -> Vec<CloudPermitV2> {
        self.contexts
            .last()
            .map(|(_, _, permits)| permits.clone())
            .unwrap_or_default()
    }
    fn tokens(&self, prefix: &str) -> usize {
        self.log
            .iter()
            .filter(|entry| entry.starts_with(&format!("token:{prefix}")))
            .count()
    }
}

struct FakeApi(Arc<Mutex<World>>);
struct FakeHost(Arc<Mutex<World>>);

fn lock(world: &Arc<Mutex<World>>) -> std::sync::MutexGuard<'_, World> {
    world.lock().unwrap_or_else(|error| error.into_inner())
}

/// The real hosted API over a transport that answers one HTTP status with a
/// body that is **not** a `DeveloperErrorV1` — a deployment refusing in its
/// own words, which is what a route it does not serve looks like from here.
/// Every request is recorded, so a test can say the answer came from the
/// route rather than from a shortcut.
fn codeless_hosted_api(world: Arc<Mutex<World>>, status: u16) -> HostedApi {
    struct Codeless {
        world: Arc<Mutex<World>>,
        status: u16,
    }
    impl koru_account_client::Transport for Codeless {
        fn send(
            &self,
            request: koru_account_client::HttpRequest,
        ) -> Result<koru_account_client::HttpResponse, koru_account_client::TransportError>
        {
            lock(&self.world).note(format!(
                "http:{:?}:{}:{}",
                request.method, request.path_and_query, self.status
            ));
            Ok(koru_account_client::HttpResponse {
                status: self.status,
                body: br#"{"detail":"this deployment does not serve that route"}"#.to_vec(),
            })
        }
    }
    HostedApi::new(koru_account_client::AccountClient::with_transport(
        Arc::new(Codeless { world, status }),
    ))
}

impl Api for FakeApi {
    fn discover(
        &mut self,
        _authority: &EnrollmentAuthorization,
        after: Option<CloudWorkspaceCursorV1>,
    ) -> Result<CloudWorkspacePageV1, Failure> {
        let mut world = lock(&self.0);
        world.note(match &after {
            None => "discover".to_owned(),
            Some(cursor) => format!("discover:{}", cursor.workspace_id.to_storage()),
        });
        discovery_failure(&mut world)?;
        let index = match after {
            None => 0,
            Some(cursor) => world
                .pages
                .iter()
                .position(|page| page.next == Some(cursor))
                .map(|position| position + 1)
                .ok_or(Failure::Unavailable)?,
        };
        let page = world
            .pages
            .get(index)
            .cloned()
            .ok_or(Failure::Unavailable)?;
        cancel_at(&mut world, index);
        Ok(page)
    }

    fn select(
        &mut self,
        _authority: &EnrollmentAuthorization,
        _profile: ProfileId,
        workspace: WorkspaceId,
    ) -> Result<(), Failure> {
        let mut world = lock(&self.0);
        world.note(format!("select:{}", workspace.to_storage()));
        if world.denied.contains(&workspace) {
            return Err(Failure::Denied);
        }
        Ok(())
    }

    fn describe(
        &mut self,
        _authority: &EnrollmentAuthorization,
        workspace: WorkspaceId,
        _profile: ProfileId,
    ) -> Result<CloudWorkspaceDescriptorV1, Failure> {
        let mut world = lock(&self.0);
        world.note(format!("describe:{}", workspace.to_storage()));
        if world.not_ready.contains(&workspace) {
            return Err(Failure::NotReady);
        }
        world
            .descriptors
            .get(&workspace)
            .copied()
            .ok_or(Failure::Unavailable)
    }

    fn issue_token(
        &mut self,
        _authority: &EnrollmentAuthorization,
        workspace: WorkspaceId,
        profile: ProfileId,
    ) -> Result<SyncTokenResponseV1, Failure> {
        let mut world = lock(&self.0);
        world.note(format!("token:{}", workspace.to_storage()));
        let index = index_of(workspace);
        let logical = world
            .issued_replica
            .get(&workspace)
            .copied()
            .unwrap_or_else(|| replica(index));
        Ok(token_for_profile(
            index,
            logical,
            world.profile_route_available,
            profile.as_uuid().as_bytes()[15],
        ))
    }

    fn jwks(&mut self) -> Result<JwksResponseV1, Failure> {
        lock(&self.0).note("jwks");
        Ok(jwks())
    }

    fn bind_catalog(
        &mut self,
        authority: &EnrollmentAuthorization,
        held: Option<CloudCatalogBindingRefV2>,
    ) -> Result<CloudCatalogBindingV2, Failure> {
        let mut world = lock(&self.0);
        world.note("bind");
        // Nothing is injected on this path: the answer is an HTTP response,
        // read by the real client and classified by the real hosted
        // mapping, so a status the client throws away cannot pass as a
        // window here.
        if let Some(status) = world.hosted_binding_status {
            let shared = self.0.clone();
            drop(world);
            return codeless_hosted_api(shared, status).bind_catalog(authority, held);
        }
        if world.v1_service {
            return Err(Failure::RouteNotFound);
        }
        if world.binding_conflict || held.is_some_and(|held| held.account_id != account()) {
            return Err(Failure::CatalogBindingConflict);
        }
        Ok(CloudCatalogBindingV2 {
            version: Version2,
            account_id: account(),
            catalog_scope_id: scope(),
            bound_at_ms: 1_000,
            binding_generation: 1,
        })
    }

    fn root_catalog(
        &mut self,
        _authority: &EnrollmentAuthorization,
        _after: Option<CloudUnitRefV2>,
    ) -> Result<CloudRootCatalogV2, Failure> {
        let mut world = lock(&self.0);
        world.note("catalog");
        Ok(CloudRootCatalogV2 {
            version: Version2,
            digest: Vec::new(),
            units: world
                .service_units
                .iter()
                .map(|(unit, frontier)| CloudRootUnitStateV2 {
                    unit: *unit,
                    frontier: frontier.clone(),
                    projection_watermark: 1,
                })
                .collect(),
            next: None,
        })
    }

    fn pull_root_unit(
        &mut self,
        _authority: &EnrollmentAuthorization,
        unit: CloudUnitRefV2,
        _frontier: Vec<u8>,
    ) -> Result<CloudRootUnitUpdatesV2, Failure> {
        let mut world = lock(&self.0);
        world.note(format!("pull:{}", unit_label(unit)));
        Ok(CloudRootUnitUpdatesV2 {
            version: Version2,
            unit,
            updates: world.service_frames.get(&unit).cloned().unwrap_or_default(),
            projection_watermark: 1,
        })
    }

    fn push_root_unit(
        &mut self,
        _authority: &EnrollmentAuthorization,
        updates: CloudRootUnitUpdatesV2,
    ) -> Result<CloudRootJoinReceiptV2, Failure> {
        let mut world = lock(&self.0);
        world.note(format!("push:{}", unit_label(updates.unit)));
        world.pushed.push(updates.unit);
        Ok(world
            .push_receipt
            .clone()
            .unwrap_or(CloudRootJoinReceiptV2::Applied {
                version: Version2,
                unit: updates.unit,
                frontier: vec![1],
                revision: 1,
                projection_watermark: 1,
                redacted_ops: 0,
            }))
    }

    fn discover_v2(
        &mut self,
        _authority: &EnrollmentAuthorization,
        after: Option<CloudDiscoveryCursorV2>,
    ) -> Result<CloudDiscoveryReceiptV2, Failure> {
        let mut world = lock(&self.0);
        world.note(match &after {
            None => "discover".to_owned(),
            Some(cursor) => format!("discover:{}", cursor.workspace_id.to_storage()),
        });
        discovery_failure(&mut world)?;
        let index = match after {
            None => 0,
            Some(cursor) => world
                .receipts
                .iter()
                .position(|page| page.next == Some(cursor))
                .map(|position| position + 1)
                .ok_or(Failure::Unavailable)?,
        };
        let page = world
            .receipts
            .get(index)
            .cloned()
            .ok_or(Failure::Unavailable)?;
        cancel_at(&mut world, index);
        Ok(page)
    }

    fn issue_token_v2(
        &mut self,
        _authority: &EnrollmentAuthorization,
        scope: CloudSyncScopeV2,
    ) -> Result<SyncTokenResponseV2, Failure> {
        let mut world = lock(&self.0);
        let label = scope_label(scope);
        world.note(format!("token:{label}"));
        if world.denied_scopes.contains(&label) {
            return Err(Failure::Denied);
        }
        let (dataset, available) = match scope {
            CloudSyncScopeV2::Workspace { workspace_id } => {
                let index = index_of(workspace_id);
                let logical = world
                    .issued_replica
                    .get(&workspace_id)
                    .copied()
                    .unwrap_or_else(|| replica(index));
                (Some(workspace_dataset(index, logical)), true)
            }
            CloudSyncScopeV2::ProfileUnits { profile_id } => (
                Some(profile_dataset(index_of_profile(profile_id))),
                world.profile_route_available,
            ),
            CloudSyncScopeV2::UserCatalog | CloudSyncScopeV2::UserPersonal { .. } => (None, true),
        };
        Ok(SyncTokenResponseV2 {
            version: Version2,
            credential: credential(),
            expires_at_seconds: world.token_expiry,
            catalog_scope_id: self::scope(),
            scope,
            authority_generation: world.authority_generation,
            dataset,
            route: if available {
                CloudRouteResultV2::Available { route: route() }
            } else {
                CloudRouteResultV2::Unavailable {
                    code: SyncUnavailableCode::SyncUnavailable,
                    retry_after_seconds: 30,
                }
            },
        })
    }

    fn describe_v2(
        &mut self,
        _authority: &EnrollmentAuthorization,
        workspace: WorkspaceId,
    ) -> Result<CloudWorkspaceDescriptorV2, Failure> {
        let mut world = lock(&self.0);
        world.note(format!("describe:{}", workspace.to_storage()));
        if world.not_ready.contains(&workspace) {
            return Err(Failure::NotReady);
        }
        world
            .descriptors_v2
            .get(&workspace)
            .copied()
            .ok_or(Failure::Unavailable)
    }
}

fn discovery_failure(world: &mut World) -> Result<(), Failure> {
    if world.discover_failures > 0 {
        world.discover_failures -= 1;
        return Err(Failure::Offline);
    }
    if world.discover_auth_failures > 0 {
        world.discover_auth_failures -= 1;
        return Err(Failure::SessionExpired);
    }
    if world.discover_access_failures > 0 {
        world.discover_access_failures -= 1;
        return Err(Failure::AccessRevoked);
    }
    Ok(())
}

fn cancel_at(world: &mut World, index: usize) {
    if world
        .cancel_discovery_page
        .as_ref()
        .is_some_and(|(at, _)| *at == index)
    {
        let (_, cancel) = world.cancel_discovery_page.take().unwrap();
        cancel.request();
    }
}

impl Host for FakeHost {
    fn local_workspace(
        &self,
        account: KoruIdentityId,
        dataset: CloudReplicaPrincipalV1,
    ) -> Result<Option<LocalBootstrap>, Failure> {
        let CloudReplicaPrincipalV1::Workspace { workspace_id, .. } = dataset else {
            panic!("the bootstrap read is only ever asked for a Workspace dataset");
        };
        assert_eq!(account, self::account());
        let workspace = WorkspaceId::from_uuid(Uuid::from_bytes(*workspace_id.as_bytes()));
        let mut world = lock(&self.0);
        world.note(format!("local:{}", workspace.to_storage()));
        Ok(world.local.get(&workspace).copied())
    }

    fn ensure_profile_presence(
        &self,
        profile: ProfileId,
        account: KoruIdentityId,
    ) -> Result<(), Failure> {
        let mut world = lock(&self.0);
        world.note(format!("profile:{}:{account}", profile.to_storage()));
        if world.removed_profiles.contains(&profile) {
            Err(Failure::Bootstrap)
        } else {
            Ok(())
        }
    }

    fn ensure_workspace_presence(&self, permit: &CloudPermitV2) -> Result<(), Failure> {
        let CloudPermitScopeV2::Workspace { descriptor } = &permit.scope else {
            panic!("the bootstrap is only ever asked for a Workspace permit");
        };
        let mut world = lock(&self.0);
        let workspace = WorkspaceId::from_uuid(descriptor.workspace_id.as_uuid());
        world.note(format!(
            "workspace:{}:{}",
            workspace.to_storage(),
            descriptor.manifest_document_id.as_uuid()
        ));
        if world.placement_pending.contains(&workspace) {
            return Err(Failure::PlacementPending);
        }
        Ok(())
    }

    fn replace_context(
        &self,
        sequence: u64,
        account: KoruIdentityId,
        account_generation: u64,
        permits: &[CloudPermitV2],
    ) -> Result<(), Failure> {
        let mut world = lock(&self.0);
        world.note(format!("context:{sequence}:{}", permits.len()));
        assert_eq!(account, self::account());
        assert!(permits.len() <= MAX_LIVE_PERMITS);
        assert!(
            permits.iter().all(|permit| permit.dataset().is_some()),
            "only dataset scopes reach the host"
        );
        world
            .live_permits
            .extend(permits.iter().map(|permit| permit.permit_id));
        assert!(
            world.live_permits.len() <= MAX_LIVE_PERMITS,
            "live ownership remains bounded across context upserts"
        );
        world
            .contexts
            .push((sequence, account_generation, permits.to_vec()));
        Ok(())
    }

    fn replace_blob_access(
        &self,
        _account: KoruIdentityId,
        _account_generation: u64,
        _access: &koru_account_wire::AccessToken,
    ) -> Result<(), Failure> {
        lock(&self.0).access_pushes += 1;
        Ok(())
    }
    fn fence(&self, permit_ids: &[Uuid]) -> Result<(), Failure> {
        let mut world = lock(&self.0);
        world.note(format!("fence:{}", permit_ids.len()));
        assert!(!permit_ids.is_empty(), "an empty fence is never sent");
        world.live_permits.retain(|id| !permit_ids.contains(id));
        world.fenced.push(permit_ids.to_vec());
        Ok(())
    }

    fn open_dataset(
        &self,
        permit: &CloudPermitV2,
        _credential: &SyncCredential,
        _route: &CloudSyncRouteV1,
        dataset: CloudReplicaPrincipalV1,
        keys: &JwksResponseV1,
        access: &SessionAccess,
    ) -> Result<SessionReport, Failure> {
        assert_eq!(keys, &jwks());
        let gate = if let CloudReplicaPrincipalV1::Workspace { workspace_id, .. } = dataset {
            lock(&self.0)
                .blocked
                .get(&WorkspaceId::from_uuid(workspace_id.as_uuid()))
                .cloned()
        } else {
            None
        };
        if let Some(gate) = gate {
            loop {
                if access.access_token().is_none() {
                    return Err(Failure::SessionExpired);
                }
                match gate.lock().unwrap().recv_timeout(Duration::from_millis(10)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        }
        let mut world = lock(&self.0);
        world.note(match dataset {
            CloudReplicaPrincipalV1::Workspace { .. } => "open:workspace".to_owned(),
            CloudReplicaPrincipalV1::Profile { .. } => "open:profile".to_owned(),
        });
        assert_eq!(permit.dataset(), Some(dataset));
        if world.open_failures > 0 {
            world.open_failures -= 1;
            return Err(Failure::Offline);
        }
        if world.expired_opens > 0 {
            world.expired_opens -= 1;
            return Ok(SessionReport {
                stage: Stage::Unavailable,
                error_code: Some(CREDENTIAL_EXPIRED),
                ..SessionReport::default()
            });
        }
        world.opened.push(dataset);
        let remote_hint = world.remote_hints > 0;
        if remote_hint {
            world.remote_hints -= 1;
        }
        Ok(SessionReport {
            stage: Stage::Settled,
            remote_hint,
            ..SessionReport::default()
        })
    }

    fn account_binding(&self) -> Result<Option<CloudAccountBindingRef>, Failure> {
        Ok(lock(&self.0).host_binding)
    }

    fn bind_account_scope(&self, binding: &CloudCatalogBindingV2) -> Result<(), Failure> {
        let mut world = lock(&self.0);
        world.note("bind-store");
        if world.host_bind_refuses {
            return Err(Failure::CatalogBindingConflict);
        }
        world.bound.push(*binding);
        Ok(())
    }

    fn root_inventory(&self) -> Result<RootInventory, Failure> {
        let mut world = lock(&self.0);
        world.note("inventory");
        Ok(RootInventory {
            units: world.local_units.clone(),
            held: 0,
        })
    }

    fn root_unit_frontier(&self, unit: JoinUnit) -> Result<Option<Vec<u8>>, Failure> {
        Ok(lock(&self.0)
            .local_frontiers
            .get(&join_label(unit))
            .cloned())
    }

    fn export_root_unit(
        &self,
        unit: JoinUnit,
        _recipient_frontier: Option<&[u8]>,
    ) -> Result<CloudRootExport, Failure> {
        let mut world = lock(&self.0);
        let label = join_label(unit);
        world.note(format!("export:{label}"));
        Ok(world
            .exports
            .get(&label)
            .cloned()
            .unwrap_or(CloudRootExport::NotFound))
    }

    fn join_root_unit(
        &self,
        account: KoruIdentityId,
        unit: JoinUnit,
        frames: &[Vec<u8>],
        advertised: Option<&[u8]>,
    ) -> Result<CloudRootJoin, Failure> {
        assert_eq!(account, self::account());
        assert!(!frames.is_empty(), "a join carries frames");
        let mut world = lock(&self.0);
        let label = join_label(unit);
        world.note(format!("join:{label}"));
        world.joined.push(label);
        Ok(CloudRootJoin::Applied {
            frontier: advertised.map(<[u8]>::to_vec).unwrap_or_else(|| vec![1]),
        })
    }
}

fn index_of(workspace: WorkspaceId) -> u8 {
    workspace.as_uuid().as_bytes()[15]
}

fn index_of_profile(profile: ProfileId) -> u8 {
    profile.as_uuid().as_bytes()[15]
}

fn placement_of(permit: &CloudPermitV2) -> Option<ProfileId> {
    permit
        .placement_profile()
        .map(|profile| ProfileId::from_uuid(profile.as_uuid()))
}

fn is_workspace(permit: &CloudPermitV2) -> bool {
    matches!(permit.scope, CloudPermitScopeV2::Workspace { .. })
}

fn page(
    candidates: Vec<CloudWorkspaceCandidateV1>,
    next: Option<&CloudWorkspaceCandidateV1>,
) -> CloudWorkspacePageV1 {
    CloudWorkspacePageV1 {
        version: Version1,
        workspaces: candidates,
        next: next.map(|candidate| CloudWorkspaceCursorV1 {
            profile_id: candidate.profile_id,
            workspace_id: candidate.workspace_id,
        }),
    }
}

struct Harness {
    control: Control,
    world: Arc<Mutex<World>>,
    clock: Arc<PauseClock>,
}

impl Harness {
    fn start(world: Arc<Mutex<World>>) -> Self {
        let clock = PauseClock::new();
        let control = Control::start_with_clock(
            FakeApi(world.clone()),
            FakeHost(world.clone()),
            clock.clone(),
            Arc::new(Steps::default()),
            true,
            |_| {},
        );
        Self {
            control,
            world,
            clock,
        }
    }
    fn log(&self) -> Vec<String> {
        lock(&self.world).log.clone()
    }
    fn wait_for(&self, condition: impl Fn(&World) -> bool) {
        wait_until(|| condition(&lock(&self.world)));
    }
    /// A port call is observable before the pass that made it has published
    /// its snapshot, so anything asserted about the snapshot waits for it.
    fn wait_snapshot(&self, condition: impl Fn(&Snapshot) -> bool) {
        wait_until(|| condition(&self.control.snapshot()));
    }
    /// Advance the injected clock past a scheduled deadline and let the worker
    /// observe it, without producing a change event of its own.
    fn tick(&self, millis: u64) {
        self.clock.advance(millis);
        self.control.poke();
    }
    /// The worker computes a deadline from the injected clock after it
    /// publishes, so a test cannot know whether its advance landed before or
    /// after that. Keep advancing until the effect appears.
    fn tick_until(&self, millis: u64, condition: impl Fn(&World) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition(&lock(&self.world)) {
            assert!(Instant::now() < deadline, "condition did not become true");
            self.tick(millis);
            std::thread::yield_now();
        }
    }
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "condition did not become true");
        std::thread::yield_now();
    }
}

/// One Workspace placed under one eligible Profile, held by Cloud only: the
/// fresh-device case under the v2 contract.
fn one_hosted_pair() -> Arc<Mutex<World>> {
    let world = World::shared();
    {
        let mut state = lock(&world);
        state.receipts = vec![receipt(
            vec![candidate_v2(1, Some(1))],
            vec![live(1)],
            CloudCoverageV2::Complete,
            None,
        )];
        state
            .descriptors_v2
            .insert(workspace(1), descriptor_v2(1, Some(1)));
    }
    world
}

/// Several Workspaces each under its own Profile, all eligible.
fn hosted(indices: impl IntoIterator<Item = u8>) -> Arc<Mutex<World>> {
    let world = World::shared();
    {
        let mut state = lock(&world);
        let indices: Vec<u8> = indices.into_iter().collect();
        state.receipts = vec![receipt(
            indices.iter().map(|i| candidate_v2(*i, Some(*i))).collect(),
            indices.iter().map(|i| live(*i)).collect(),
            CloudCoverageV2::Complete,
            None,
        )];
        for i in indices {
            state
                .descriptors_v2
                .insert(workspace(i), descriptor_v2(i, Some(i)));
        }
    }
    world
}

// ----- the pass -----------------------------------------------------------

#[test]
fn a_fresh_device_binds_joins_root_units_then_admits_content_and_profile_units() {
    let harness = Harness::start(one_hosted_pair());
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 2);

    let log = harness.log();
    let expected = [
        "bind".to_owned(),
        "bind-store".to_owned(),
        "token:user_catalog".to_owned(),
        "inventory".to_owned(),
        "catalog".to_owned(),
        "discover".to_owned(),
        "jwks".to_owned(),
        "token:workspace:1".to_owned(),
        format!("local:{}", workspace(1).to_storage()),
        format!("describe:{}", workspace(1).to_storage()),
        format!("workspace:{}:{}", workspace(1).to_storage(), manifest(1)),
        format!("profile:{}:{}", profile(1).to_storage(), account()),
        "token:profile_units:1".to_owned(),
        "context:1:2".to_owned(),
    ];
    assert_eq!(&log[..expected.len()], &expected[..], "{log:?}");
    let opens: BTreeSet<&str> = log[expected.len()..].iter().map(String::as_str).collect();
    assert_eq!(opens, BTreeSet::from(["open:workspace", "open:profile"]));

    let world = lock(&harness.world);
    assert!(world.fenced.is_empty(), "nothing was live to fence");
    assert_eq!(world.bound.len(), 1);
    assert_eq!(world.bound[0].catalog_scope_id, scope());
    let permits = world.permits();
    assert_eq!(permits.len(), 2);
    let content = permits.iter().find(|permit| is_workspace(permit)).unwrap();
    let units = permits.iter().find(|permit| !is_workspace(permit)).unwrap();
    assert_eq!(content.version, 2);
    assert_eq!(content.dataset(), Some(workspace_dataset(1, replica(1))));
    assert_eq!(content.catalog_scope_id, Some(cloud_id(scope().as_uuid())));
    assert_eq!(placement_of(content), Some(profile(1)));
    assert_eq!(content.expires_at_seconds, 4_000);
    let CloudPermitScopeV2::Workspace { descriptor } = &content.scope else {
        unreachable!()
    };
    assert_eq!(descriptor.manifest_document_id, cloud_id(manifest(1)));
    assert_eq!(units.dataset(), Some(profile_dataset(1)));
    let CloudPermitScopeV2::ProfileUnits {
        allowed_profile_units,
        ..
    } = &units.scope
    else {
        unreachable!()
    };
    assert_eq!(
        allowed_profile_units,
        &vec![
            ProfileUnitRef::Identity {
                profile: profile(1)
            },
            ProfileUnitRef::Private {
                profile: profile(1)
            },
        ]
    );
    assert_ne!(content.permit_id, units.permit_id);
    drop(world);

    // Progress snapshots expose both records before their sessions settle.
    harness
        .wait_snapshot(|snapshot| snapshot.records.len() == 2 && snapshot.stage == Stage::Settled);
    let snapshot = harness.control.snapshot();
    assert_eq!(snapshot.version, 1);
    assert_eq!(snapshot.sequence, 1);
    assert_eq!(snapshot.account_id, Some(account()));
    assert_eq!(snapshot.contract, Some(CloudContract::V2));
    assert_eq!(snapshot.catalog_scope_id, Some(scope()));
    assert_eq!(snapshot.records.len(), 2);
    assert!(snapshot.records.iter().all(|r| r.stage == Stage::Settled));
    assert!(
        snapshot
            .records
            .iter()
            .all(|r| r.profile_id == Some(profile(1)))
    );
    assert_eq!(snapshot.stage, Stage::Settled);
    assert_eq!(snapshot.failure_code, None);
    assert!(snapshot.deferred.is_empty());
    // Sorted by permit id, as CloudStatus requires.
    let ids: Vec<_> = snapshot.records.iter().map(|r| r.permit_id).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
}

/// The incident scenario (TECHNICAL.final §2, §7.3): W was moved from A to
/// B and A was deleted. The receipt places W under B and lists A as
/// tombstoned; this device no longer holds A. W's content is admitted on its
/// own identity, nothing asks A to be present, B's units are admitted
/// independently, and A's never are.
#[test]
fn a_moved_workspace_bootstraps_without_its_former_profile() {
    let world = World::shared();
    {
        let mut state = lock(&world);
        state.receipts = vec![receipt(
            vec![candidate_v2(1, Some(2))],
            vec![profile_result(1, false, SubjectLifecycle::Deleted), live(2)],
            CloudCoverageV2::Complete,
            None,
        )];
        state
            .descriptors_v2
            .insert(workspace(1), descriptor_v2(1, Some(2)));
        state.removed_profiles.insert(profile(1));
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 2);
    let log = harness.log();
    assert!(
        !log.iter()
            .any(|entry| entry.starts_with(&format!("profile:{}", profile(1).to_storage()))),
        "the former Profile is never asked to be present: {log:?}"
    );
    assert!(
        log.contains(&format!(
            "profile:{}:{}",
            profile(2).to_storage(),
            account()
        )),
        "the current Profile's units are admitted independently: {log:?}"
    );
    let world = lock(&harness.world);
    assert!(world.opened.contains(&workspace_dataset(1, replica(1))));
    assert!(world.opened.contains(&profile_dataset(2)));
    assert!(!world.opened.contains(&profile_dataset(1)));
    let permits = world.permits();
    assert!(
        permits
            .iter()
            .all(|permit| placement_of(permit) == Some(profile(2)))
    );
    drop(world);
    harness.wait_snapshot(|snapshot| snapshot.stage == Stage::Settled);
    let receipt = harness.control.snapshot().discovery.unwrap();
    assert_eq!(receipt.for_profile(profile(2)).discovered, 1);
    assert_eq!(receipt.for_profile(profile(1)).discovered, 0);
    assert_eq!(
        receipt.for_profile(profile(1)).profile_lifecycle.as_deref(),
        Some("deleted")
    );
}

/// §4.4: a Cloud Workspace moved into a Profile whose ceiling closes the
/// Cloud path keeps its choice and stops syncing; the device's own policy
/// is the last admission input and is reported, never overridden.
#[test]
fn the_current_profiles_mode_ceiling_still_stops_transport() {
    let world = one_hosted_pair();
    lock(&world).local.insert(
        workspace(1),
        LocalBootstrap {
            workspace_id: workspace(1),
            manifest_document_id: manifest(1),
            allows_cloud: Some(false),
        },
    );
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.failure_code == Some("mode_refuses_transport"));
    let state = lock(&harness.world);
    assert!(
        state.opened.is_empty(),
        "nothing opens under a refused policy"
    );
    assert!(
        state
            .contexts
            .iter()
            .all(|(_, _, permits)| permits.is_empty()),
        "no permit is installed for a refused Workspace, and none for the Profile it would have made eligible"
    );
    assert!(
        !state.log.iter().any(|entry| entry.starts_with("describe:")),
        "a held Workspace needs no hosted lookup: {:?}",
        state.log
    );
    drop(state);
    let snapshot = harness.control.snapshot();
    assert_eq!(snapshot.deferred.len(), 1);
    assert_eq!(snapshot.deferred[0].workspace_id, Some(workspace(1)));
    assert_eq!(snapshot.deferred[0].profile_id, Some(profile(1)));
    assert_eq!(snapshot.deferred[0].code, "mode_refuses_transport");
    assert!(
        snapshot.retry_at_millis.is_some(),
        "a mode change is retried"
    );
}

/// A credential already inside its renewal window moves no root unit, and
/// a receipt from a newer authorization generation fences every admission
/// issued under the older one before anything is opened under it again.
#[test]
fn an_expired_or_older_generation_permit_sends_nothing() {
    // Expired: the service holds a Workspace unit this device lacks, and
    // this device holds a Profile unit the service lacks; the catalog
    // credential the exchange would run under is already stale.
    let world = one_hosted_pair();
    {
        let mut state = lock(&world);
        state.token_expiry = 50;
        state.service_units.insert(
            CloudUnitRefV2::WorkspaceCatalog {
                subject_id: workspace(1),
            },
            vec![7],
        );
        state.service_frames.insert(
            CloudUnitRefV2::WorkspaceCatalog {
                subject_id: workspace(1),
            },
            vec![vec![1, 2, 3]],
        );
        state.local_units.push(CloudRootUnitState {
            unit: JoinUnit::ProfileCatalog {
                profile: profile(1),
            },
            frontier: vec![9],
        });
        state.exports.insert(
            "profile:1".into(),
            CloudRootExport::Published {
                frames: vec![vec![4]],
                redacted_ops: 0,
            },
        );
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.failure_code == Some(CREDENTIAL_EXPIRED));
    let state = lock(&harness.world);
    assert!(
        !state
            .log
            .iter()
            .any(|entry| entry.starts_with("pull:") || entry.starts_with("push:")),
        "no metadata moves under an expired credential: {:?}",
        state.log
    );
    assert!(state.joined.is_empty());
    assert!(state.pushed.is_empty());
    assert!(state.opened.is_empty(), "no content opens under it either");
    drop(state);

    // Older generation: a fresh pass admits under generation 1; the next
    // receipt is read under generation 2, so the held admissions are stale
    // and are fenced and re-issued before their sessions run.
    let harness = Harness::start(one_hosted_pair());
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 2);
    let first = lock(&harness.world).permits();
    {
        let mut state = lock(&harness.world);
        state.receipts[0].authorization_generation = 2;
        state.authority_generation = 2;
    }
    harness.control.wake();
    harness.tick_until(CHANGE_DEBOUNCE.as_millis() as u64, |world| {
        world.contexts.len() == 2
    });
    harness.wait_for(|world| world.opened.len() == 4);
    let state = lock(&harness.world);
    let fenced: BTreeSet<Uuid> = state.fenced.concat().into_iter().collect();
    assert_eq!(
        fenced,
        first.iter().map(|permit| permit.permit_id).collect(),
        "every admission of the older generation is fenced"
    );
    let renewed = state.permits();
    assert!(renewed.iter().all(|permit| permit.authority_generation == 2
        && !first.iter().any(|old| old.permit_id == permit.permit_id)));
}

/// §4.1: a device bound to another account keeps its local work and reports
/// the conflict as a terminal failure of this account. Whether the service
/// or the Store refuses, nothing else of the account is asked for.
#[test]
fn a_binding_conflict_is_terminal_and_leaves_the_local_catalog_alone() {
    for store_refuses in [false, true] {
        let world = one_hosted_pair();
        {
            let mut state = lock(&world);
            state.host_binding = Some(CloudAccountBindingRef {
                account_id: cloud_id(other_account().as_uuid()),
                catalog_scope_id: cloud_id(scope().as_uuid()),
            });
            if store_refuses {
                // The service did not notice; the Store does.
                state.binding_conflict = false;
                state.host_binding = None;
                state.host_bind_refuses = true;
            }
        }
        let harness = Harness::start(world);
        harness.control.update_authorization(authority("1"));
        harness.wait_snapshot(|snapshot| snapshot.failure_code == Some(CATALOG_BINDING_CONFLICT));
        let snapshot = harness.control.snapshot();
        assert_eq!(snapshot.retry_at_millis, None, "terminal: no retry");
        assert_eq!(snapshot.contract, None);
        let state = lock(&harness.world);
        assert!(state.bound.is_empty(), "the local binding is untouched");
        assert!(
            !state.log.iter().any(|entry| entry.starts_with("token:")
                || entry.starts_with("discover")
                || entry.starts_with("catalog")),
            "nothing of the account is asked for after the conflict: {:?}",
            state.log
        );
        assert!(state.contexts.is_empty());
    }
}

/// §4.4 at one revision: the Profile that loses its last Cloud Workspace
/// loses its Identity/Private admission and the destination gains one in
/// the same pass; the moved Workspace is counted under its destination and
/// nowhere else.
#[test]
fn moving_the_last_cloud_workspace_closes_the_profiles_units_and_opens_the_destinations() {
    let world = World::shared();
    {
        let mut state = lock(&world);
        state.receipts = vec![receipt(
            vec![candidate_v2(1, Some(1))],
            vec![live(1), profile_result(2, false, SubjectLifecycle::Live)],
            CloudCoverageV2::Complete,
            None,
        )];
        state
            .descriptors_v2
            .insert(workspace(1), descriptor_v2(1, Some(1)));
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 2);
    let before = lock(&harness.world).permits();
    let old_units = before
        .iter()
        .find(|permit| !is_workspace(permit))
        .unwrap()
        .clone();
    assert_eq!(placement_of(&old_units), Some(profile(1)));

    // The move lands: W is placed under Profile 2 at a later revision, and
    // the projection says Profile 1 is no longer eligible.
    {
        let mut state = lock(&harness.world);
        let mut moved = candidate_v2(1, Some(2));
        moved.placement.placement_revision = 2;
        state.receipts = vec![receipt(
            vec![moved],
            vec![profile_result(1, false, SubjectLifecycle::Live), live(2)],
            CloudCoverageV2::Complete,
            None,
        )];
        let mut description = descriptor_v2(1, Some(2));
        description.placement.placement_revision = 2;
        state.descriptors_v2.insert(workspace(1), description);
    }
    harness.control.wake();
    harness.tick_until(CHANGE_DEBOUNCE.as_millis() as u64, |world| {
        world.contexts.len() == 2
    });
    harness.wait_for(|world| world.opened.contains(&profile_dataset(2)));
    harness.wait_snapshot(|snapshot| snapshot.discovery.as_ref().is_some_and(|d| !d.stale));
    let state = lock(&harness.world);
    assert!(
        state.fenced.concat().contains(&old_units.permit_id),
        "the former Profile's units admission is fenced"
    );
    let after = state.permits();
    assert_eq!(after.len(), 2);
    assert!(
        after
            .iter()
            .all(|permit| placement_of(permit) == Some(profile(2)))
    );
    drop(state);
    let receipt = harness.control.snapshot().discovery.unwrap();
    assert_eq!(receipt.for_profile(profile(1)).discovered, 0);
    assert_eq!(receipt.for_profile(profile(2)).discovered, 1);
    assert_eq!(
        receipt.profiles.values().map(|c| c.discovered).sum::<u32>()
            + receipt.unassigned.discovered,
        1,
        "a moved Workspace is counted once"
    );
}

/// The compatibility window: a service that answers the binding route with
/// 404 is served the v1 pair path unchanged — selection, the Profile
/// presence check, the Profile-first bootstrap — and the pair is read as
/// the same two scopes the v2 path produces.
#[test]
fn a_v1_service_runs_the_pair_path_once_and_yields_the_same_two_scopes() {
    let world = World::shared();
    {
        let mut state = lock(&world);
        state.v1_service = true;
        state.pages = vec![page(vec![candidate(1)], None)];
        state.descriptors.insert(workspace(1), descriptor(1));
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 2);
    let log = harness.log();
    let expected = [
        "bind".to_owned(),
        "discover".to_owned(),
        "jwks".to_owned(),
        format!("select:{}", workspace(1).to_storage()),
        format!("token:{}", workspace(1).to_storage()),
        format!("local:{}", workspace(1).to_storage()),
        format!("describe:{}", workspace(1).to_storage()),
        format!("profile:{}:{}", profile(1).to_storage(), account()),
        format!("workspace:{}:{}", workspace(1).to_storage(), manifest(1)),
        "context:1:2".to_owned(),
    ];
    assert_eq!(&log[..expected.len()], &expected[..], "{log:?}");
    assert!(
        !log.iter()
            .any(|entry| entry == "inventory" || entry == "catalog"),
        "no root exchange against a v1 service: {log:?}"
    );
    let state = lock(&harness.world);
    assert!(state.bound.is_empty());
    let permits = state.permits();
    assert_eq!(permits.len(), 2);
    assert!(
        permits
            .iter()
            .all(|permit| permit.catalog_scope_id.is_none())
    );
    assert!(
        permits
            .iter()
            .all(|permit| permit.authority_generation == 1
                && placement_of(permit) == Some(profile(1)))
    );
    let units = permits.iter().find(|permit| !is_workspace(permit)).unwrap();
    let CloudPermitScopeV2::ProfileUnits {
        allowed_profile_units,
        ..
    } = &units.scope
    else {
        unreachable!()
    };
    assert_eq!(
        allowed_profile_units.len(),
        2,
        "Identity and Private; no Personal"
    );
    drop(state);
    harness.wait_snapshot(|snapshot| snapshot.contract == Some(CloudContract::V1));
    assert!(harness.control.snapshot().catalog_scope_id.is_none());
}

/// The same window, driven the way the live service drives it: a genuine
/// 404 HTTP answer whose body is not Koru's error envelope, through the real
/// account client and the real hosted mapping. No `Failure` is injected —
/// the fake's own `RouteNotFound` is never reached — because the defect this
/// covers lived entirely between the status and that enum. The client
/// discarded the status of any non-2xx it could not read as a
/// `DeveloperErrorV1`, the mapping saw `InvalidResponse` and called it
/// unavailable, the contract probe never chose `V1`, and the pass settled
/// with no records at all: Cloud unavailable on every Profile, with nothing
/// syncing, against a service the window exists to keep working with.
#[test]
fn a_404_in_the_services_own_words_engages_the_v1_window() {
    let world = World::shared();
    {
        let mut state = lock(&world);
        state.hosted_binding_status = Some(404);
        state.pages = vec![page(vec![candidate(1)], None)];
        state.descriptors.insert(workspace(1), descriptor(1));
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 2);
    harness.wait_snapshot(|snapshot| snapshot.contract == Some(CloudContract::V1));
    let log = harness.log();
    assert!(
        log.contains(&"http:Put:/v1/account/cloud/catalog/binding:404".to_owned()),
        "the window came from a 404 on the binding route itself: {log:?}"
    );
    // The pair path, unchanged, with the HTTP exchange of the binding route
    // taken out of the sequence it is interleaved with.
    let hosted: Vec<String> = log
        .iter()
        .filter(|entry| !entry.starts_with("http:"))
        .cloned()
        .collect();
    let expected = [
        "bind".to_owned(),
        "discover".to_owned(),
        "jwks".to_owned(),
        format!("select:{}", workspace(1).to_storage()),
        format!("token:{}", workspace(1).to_storage()),
        format!("local:{}", workspace(1).to_storage()),
        format!("describe:{}", workspace(1).to_storage()),
        format!("profile:{}:{}", profile(1).to_storage(), account()),
        format!("workspace:{}:{}", workspace(1).to_storage(), manifest(1)),
        "context:1:2".to_owned(),
    ];
    assert_eq!(&hosted[..expected.len()], &expected[..], "{hosted:?}");
    assert!(
        !log.iter()
            .any(|entry| entry == "inventory" || entry == "catalog"),
        "no root exchange against a v1 service: {log:?}"
    );
    let state = lock(&harness.world);
    assert!(state.bound.is_empty(), "nothing is bound in the window");
    assert_eq!(
        state.permits().len(),
        2,
        "content and Profile units admitted"
    );
    drop(state);
    let snapshot = harness.control.snapshot();
    assert_eq!(snapshot.failure_code, None);
    assert_eq!(snapshot.records.len(), 2);
}

/// A missing route is a 404 and nothing else. A service that is simply
/// unwell answers 503 in its own words too, and that is not a contract
/// reading: the pass reports it and retries, the generation keeps no
/// contract, and no content is admitted under a window the service never
/// asked for.
#[test]
fn another_codeless_status_is_not_read_as_a_missing_route() {
    let world = World::shared();
    {
        let mut state = lock(&world);
        state.hosted_binding_status = Some(503);
        state.pages = vec![page(vec![candidate(1)], None)];
        state.descriptors.insert(workspace(1), descriptor(1));
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.failure_code == Some(CLOUD_UNAVAILABLE));
    let snapshot = harness.control.snapshot();
    assert_eq!(snapshot.contract, None);
    assert!(snapshot.records.is_empty());
    let log = harness.log();
    assert!(
        log.contains(&"http:Put:/v1/account/cloud/catalog/binding:503".to_owned()),
        "{log:?}"
    );
    assert!(
        !log.iter().any(|entry| entry == "discover"),
        "a pass that cannot read the contract admits nothing: {log:?}"
    );
}

/// The root-unit join runs before any admission: Profile units then
/// Workspace units are pulled from the service and pushed from the device,
/// and the service's `Pending` is answered once more from its frontier.
#[test]
fn root_units_are_exchanged_before_content_profile_units_first() {
    let world = one_hosted_pair();
    {
        let mut state = lock(&world);
        for (unit, label) in [
            (
                CloudUnitRefV2::WorkspaceCatalog {
                    subject_id: workspace(1),
                },
                "workspace:1",
            ),
            (
                CloudUnitRefV2::ProfileCatalog {
                    subject_id: profile(1),
                },
                "profile:1",
            ),
        ] {
            state.service_units.insert(unit, vec![7]);
            state.service_frames.insert(unit, vec![vec![1]]);
            state.local_frontiers.insert(label.into(), vec![3]);
        }
        // Locally this device also holds Profile 2's unit, which Cloud lacks.
        state.local_units.push(CloudRootUnitState {
            unit: JoinUnit::ProfileCatalog {
                profile: profile(2),
            },
            frontier: vec![5],
        });
        state.exports.insert(
            "profile:2".into(),
            CloudRootExport::Published {
                frames: vec![vec![9]],
                redacted_ops: 1,
            },
        );
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 2);
    let log = harness.log();
    let position = |entry: &str| {
        log.iter()
            .position(|e| e == entry)
            .unwrap_or_else(|| panic!("{entry} in {log:?}"))
    };
    assert!(position("pull:profile:1") < position("pull:workspace:1"));
    assert!(position("join:workspace:1") < position("discover"));
    assert!(position("push:profile:2") < position("discover"));
    let state = lock(&harness.world);
    assert_eq!(state.joined, vec!["profile:1", "workspace:1"]);
    assert_eq!(
        state.pushed,
        vec![CloudUnitRefV2::ProfileCatalog {
            subject_id: profile(2)
        }]
    );
}

#[test]
fn a_held_export_and_a_pending_push_never_send_raw_bytes() {
    let world = one_hosted_pair();
    {
        let mut state = lock(&world);
        state.local_units.push(CloudRootUnitState {
            unit: JoinUnit::WorkspaceCatalog {
                workspace: workspace(1),
            },
            frontier: vec![5],
        });
        state
            .exports
            .insert("workspace:1".into(), CloudRootExport::Held);
        state.local_units.push(CloudRootUnitState {
            unit: JoinUnit::ProfileCatalog {
                profile: profile(1),
            },
            frontier: vec![5],
        });
        state.exports.insert(
            "profile:1".into(),
            CloudRootExport::Published {
                frames: vec![vec![1]],
                redacted_ops: 0,
            },
        );
        state.push_receipt = Some(CloudRootJoinReceiptV2::Pending {
            version: Version2,
            unit: CloudUnitRefV2::ProfileCatalog {
                subject_id: profile(1),
            },
            frontier: vec![2],
        });
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 2);
    let state = lock(&harness.world);
    assert!(
        !state.pushed.contains(&CloudUnitRefV2::WorkspaceCatalog {
            subject_id: workspace(1)
        }),
        "a held unit is not pushed"
    );
    assert_eq!(
        state
            .pushed
            .iter()
            .filter(|unit| matches!(unit, CloudUnitRefV2::ProfileCatalog { .. }))
            .count(),
        2,
        "a pending receipt is answered once more from the service's frontier, then left"
    );
    assert_eq!(
        state
            .log
            .iter()
            .filter(|e| e.as_str() == "export:profile:1")
            .count(),
        2
    );
}

/// A Workspace whose placement this device cannot list it under yet is
/// deferred with `placement_pending` and retried; it is not a broken
/// bootstrap and no permit is invented for it.
#[test]
fn a_pending_placement_is_retried_after_the_next_root_join() {
    let world = one_hosted_pair();
    lock(&world).placement_pending.insert(workspace(1));
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.failure_code == Some(PLACEMENT_PENDING));
    assert!(lock(&harness.world).opened.is_empty());
    assert!(harness.control.snapshot().retry_at_millis.is_some());
    lock(&harness.world).placement_pending.clear();
    harness.tick(1_000);
    harness.wait_for(|world| world.opened.len() == 2);
}

/// The account's personal unit for a Workspace follows the Workspace's
/// content: admitted beside it under its own scope, exchanged after the
/// content sessions, never under a Profile permit.
#[test]
fn the_personal_unit_follows_the_workspace_after_its_content() {
    let world = one_hosted_pair();
    {
        let mut state = lock(&world);
        let unit = CloudUnitRefV2::PersonalV2 {
            subject_id: workspace(1),
        };
        state.service_units.insert(unit, vec![7]);
        state.service_frames.insert(unit, vec![vec![1]]);
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.joined.contains(&"personal:1".to_owned()));
    let log = harness.log();
    let position = |entry: &str| {
        log.iter()
            .position(|e| e == entry)
            .unwrap_or_else(|| panic!("{entry} in {log:?}"))
    };
    assert!(position("token:user_personal:1") > position("token:workspace:1"));
    assert!(position("pull:personal:1") > position("open:workspace"));
    let state = lock(&harness.world);
    assert_eq!(
        state.permits().len(),
        2,
        "the personal admission never reaches the host"
    );
}

#[test]
fn a_blocked_workspace_does_not_hold_another_workspaces_session() {
    let world = hosted([1, 2]);
    let (release, wait) = mpsc::channel();
    lock(&world)
        .blocked
        .insert(workspace(1), Arc::new(Mutex::new(wait)));
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.contains(&profile_dataset(2)));
    harness.wait_for(|world| world.opened.contains(&workspace_dataset(2, replica(2))));
    assert!(
        !lock(&harness.world)
            .opened
            .contains(&workspace_dataset(1, replica(1)))
    );
    release.send(()).unwrap();
    harness.wait_for(|world| world.opened.len() == 4);
}

#[test]
fn a_profile_refills_beyond_the_first_cohort_while_another_remains_blocked() {
    let world = World::shared();
    let (release, wait) = mpsc::channel();
    {
        let mut state = lock(&world);
        let candidates = (1..=20)
            .map(|index| {
                let placement = if index == 1 { 1 } else { 2 };
                state
                    .descriptors_v2
                    .insert(workspace(index), descriptor_v2(index, Some(placement)));
                candidate_v2(index, Some(placement))
            })
            .collect();
        state.receipts = vec![receipt(
            candidates,
            vec![live(1), live(2)],
            CloudCoverageV2::Complete,
            None,
        )];
        state
            .blocked
            .insert(workspace(1), Arc::new(Mutex::new(wait)));
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.contains(&workspace_dataset(20, replica(20))));
    assert!(
        !lock(&harness.world)
            .opened
            .contains(&workspace_dataset(1, replica(1)))
    );
    release.send(()).unwrap();
    // Twenty Workspaces and the two Profiles' units.
    harness.wait_for(|world| world.opened.len() == 22);
}

#[test]
fn unchanged_discovery_keeps_permits_and_backgrounding_retires_them() {
    let harness = Harness::start(one_hosted_pair());
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|s| s.stage == Stage::Settled && s.records.len() == 2);
    let first = lock(&harness.world).permits();
    harness.control.wake();
    harness.tick_until(500, |w| w.contexts.len() == 2);
    harness.wait_for(|w| w.opened.len() == 4);
    {
        let world = lock(&harness.world);
        assert_eq!(world.permits(), first);
        assert!(world.fenced.is_empty());
        assert_eq!(world.tokens("workspace:"), 1);
        assert_eq!(world.tokens("profile_units:"), 1);
    }
    harness.control.set_foreground(false);
    harness.wait_for(|w| w.live_permits.is_empty());
    harness.control.set_foreground(true);
    harness.wait_for(|w| w.contexts.len() == 3);
    assert_ne!(lock(&harness.world).permits(), first);
}

#[test]
fn a_slow_workspace_does_not_block_a_sibling_in_the_same_profile() {
    let world = one_hosted_pair();
    let (release, wait) = mpsc::channel();
    {
        let mut state = lock(&world);
        state.receipts[0].results.push(candidate_v2(2, Some(1)));
        state
            .descriptors_v2
            .insert(workspace(2), descriptor_v2(2, Some(1)));
        state
            .blocked
            .insert(workspace(1), Arc::new(Mutex::new(wait)));
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|w| w.opened.contains(&workspace_dataset(2, replica(2))));
    assert!(
        !lock(&harness.world)
            .opened
            .contains(&workspace_dataset(1, replica(1)))
    );
    release.send(()).unwrap();
    // Two Workspaces and one Profile's units.
    harness.wait_for(|w| w.opened.len() == 3);
}

#[test]
fn without_an_authorization_the_coordinator_is_idle() {
    let harness = Harness::start(one_hosted_pair());
    harness.control.wake();
    harness.control.set_foreground(true);
    std::thread::sleep(Duration::from_millis(30));
    assert!(
        harness.log().is_empty(),
        "no port is touched without authority"
    );
    let snapshot = harness.control.snapshot();
    assert_eq!(snapshot.stage, Stage::AwaitingAuthority);
    assert_eq!(snapshot.stage.as_str(), "awaiting_authority");
    assert!(snapshot.records.is_empty());
    assert_eq!(snapshot.account_id, None);
}

#[test]
fn progress_is_published_before_blocking_dataset_sessions() {
    let world = one_hosted_pair();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observer = observed.clone();
    let observed_world = world.clone();
    let control = Control::start_with_clock(
        FakeApi(world.clone()),
        FakeHost(world.clone()),
        PauseClock::new(),
        Arc::new(Steps::default()),
        true,
        move |snapshot| {
            observer
                .lock()
                .unwrap()
                .push((lock(&observed_world).opened.len(), snapshot));
        },
    );
    control.update_authorization(authority("1"));
    wait_until(|| {
        observed
            .lock()
            .unwrap()
            .iter()
            .any(|(opened, snapshot)| *opened == 2 && snapshot.stage == Stage::Settled)
    });
    let snapshots = observed.lock().unwrap();
    assert!(
        snapshots
            .iter()
            .any(|(opened, snapshot)| *opened == 0 && snapshot.stage == Stage::Connecting)
    );
    assert!(snapshots.iter().any(|(opened, snapshot)| {
        *opened == 0
            && snapshot.records.iter().any(|record| {
                record.profile_id == Some(profile(1)) && record.stage == Stage::Reconciling
            })
    }));
}

#[test]
fn an_originating_device_uses_its_own_manifest_identity_and_skips_the_hosted_lookup() {
    let world = one_hosted_pair();
    lock(&world).local.insert(
        workspace(1),
        LocalBootstrap {
            workspace_id: workspace(1),
            manifest_document_id: manifest(9),
            allows_cloud: Some(true),
        },
    );
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 2);

    let log = harness.log();
    assert!(
        !log.iter().any(|entry| entry.starts_with("describe:")),
        "the originating device never asks the hosted lookup: {log:?}"
    );
    assert!(
        !log.iter().any(|entry| entry.starts_with("workspace:")),
        "a Workspace this device holds is not provisioned again: {log:?}"
    );
    let permits = lock(&harness.world).permits();
    let content = permits.iter().find(|permit| is_workspace(permit)).unwrap();
    let CloudPermitScopeV2::Workspace { descriptor } = &content.scope else {
        unreachable!()
    };
    assert_eq!(descriptor.manifest_document_id, cloud_id(manifest(9)));
    assert_eq!(descriptor.logical_replica_id, cloud_id(replica(1)));
    assert_eq!(descriptor.lineage_id, cloud_id(incarnation(1)));
    assert_eq!(descriptor.membership_epoch, 5);
    assert_eq!(descriptor.placement, Some(cloud_id(profile(1).as_uuid())));
}

/// A removed Profile stops its own Identity/Private units, and nothing
/// else: the Workspaces placed under it still sync on their own identity.
#[test]
fn a_removed_profile_loses_its_units_but_not_its_workspaces_content() {
    let world = hosted([1, 2]);
    lock(&world).removed_profiles.insert(profile(1));
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 3);
    let state = lock(&harness.world);
    assert!(state.opened.contains(&workspace_dataset(1, replica(1))));
    assert!(state.opened.contains(&workspace_dataset(2, replica(2))));
    assert!(state.opened.contains(&profile_dataset(2)));
    assert!(!state.opened.contains(&profile_dataset(1)));
    drop(state);
    harness.wait_snapshot(|snapshot| snapshot.failure_code == Some(BOOTSTRAP_FAILED));
    let snapshot = harness.control.snapshot();
    assert_eq!(
        snapshot.deferred,
        vec![Deferred {
            profile_id: Some(profile(1)),
            workspace_id: None,
            code: BOOTSTRAP_FAILED,
        }]
    );
}

#[test]
fn a_descriptor_that_is_not_ready_skips_the_workspace_and_retries() {
    let world = one_hosted_pair();
    lock(&world).not_ready.insert(workspace(1));
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.failure_code == Some(DESCRIPTOR_NOT_READY));

    {
        let state = lock(&harness.world);
        assert_eq!(state.contexts[0].2.len(), 0, "no permit is invented");
        assert!(state.opened.is_empty());
    }
    let snapshot = harness.control.snapshot();
    assert_eq!(snapshot.failure_code, Some(DESCRIPTOR_NOT_READY));
    assert_eq!(snapshot.deferred.len(), 1);
    assert_eq!(snapshot.deferred[0].workspace_id, Some(workspace(1)));
    assert_eq!(snapshot.retry_at_millis, Some(2_000));

    // The retry happens once the backoff is due, and the mapping that has
    // appeared meanwhile is used without any restart.
    lock(&harness.world).not_ready.clear();
    harness.tick(1_000);
    harness.wait_for(|world| world.opened.len() == 2);
    harness.wait_snapshot(|snapshot| snapshot.failure_code.is_none());
}

#[test]
fn a_denied_workspace_token_defers_that_workspace_and_admits_the_other() {
    let world = hosted([1, 2]);
    lock(&world).denied_scopes.insert("workspace:1".into());
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.failure_code == Some(SELECTION_DENIED));

    let log = harness.log();
    assert!(
        !log.contains(&format!("local:{}", workspace(1).to_storage())),
        "a denied Workspace never reaches the bootstrap step: {log:?}"
    );
    let permits = lock(&harness.world).permits();
    assert_eq!(permits.len(), 2);
    assert!(
        permits
            .iter()
            .all(|permit| placement_of(permit) == Some(profile(2)))
    );
    let snapshot = harness.control.snapshot();
    assert_eq!(snapshot.deferred.len(), 1);
    assert_eq!(snapshot.deferred[0].code, SELECTION_DENIED);
    assert_eq!(snapshot.failure_code, Some(SELECTION_DENIED));
}

#[test]
fn a_credential_naming_another_replica_is_refused() {
    let world = one_hosted_pair();
    lock(&world).issued_replica.insert(workspace(1), replica(8));
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.failure_code == Some(DATASET_MISMATCH));

    let state = lock(&harness.world);
    assert!(state.contexts[0].2.is_empty(), "no permit is admitted");
    assert!(state.opened.is_empty());
    drop(state);
    let snapshot = harness.control.snapshot();
    assert_eq!(snapshot.failure_code, Some(DATASET_MISMATCH));
    assert_eq!(snapshot.deferred[0].code, DATASET_MISMATCH);
}

#[test]
fn an_unavailable_dataset_route_is_reported_rather_than_invented() {
    let world = one_hosted_pair();
    lock(&world).profile_route_available = false;
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.stage == Stage::Unavailable);

    let snapshot = harness.control.snapshot();
    assert_eq!(lock(&harness.world).opened.len(), 1);
    assert_eq!(snapshot.records.len(), 2);
    let profile_record = snapshot
        .records
        .iter()
        .find(|record| matches!(record.dataset, CloudReplicaPrincipalV1::Profile { .. }))
        .expect("profile record");
    assert_eq!(profile_record.stage, Stage::Unavailable);
    assert_eq!(profile_record.error_code, Some(WORKER_UNAVAILABLE));
    assert_eq!(snapshot.stage, Stage::Unavailable);
    assert!(snapshot.retry_at_millis.is_some(), "the worker is retried");
}

#[test]
fn replacing_the_authorization_fences_every_permit_and_bumps_the_generation() {
    let harness = Harness::start(one_hosted_pair());
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.records.len() == 2);
    let first = lock(&harness.world).permits();
    assert_eq!(first[0].account_generation, 1);

    harness.control.update_authorization(authority("2"));
    harness.wait_for(|world| world.contexts.len() == 2);

    let state = lock(&harness.world);
    let fenced: BTreeSet<Uuid> = state.fenced.concat().into_iter().collect();
    assert_eq!(
        fenced,
        first.iter().map(|permit| permit.permit_id).collect(),
        "every permit of the replaced account is fenced"
    );
    let (sequence, generation, permits) = state.contexts[1].clone();
    assert_eq!(sequence, 2, "the sequence only ever increases");
    assert_eq!(generation, 2);
    assert!(permits.iter().all(|permit| permit.account_generation == 2));
    assert!(
        permits
            .iter()
            .all(|permit| !first.iter().any(|old| old.permit_id == permit.permit_id)),
        "a replaced generation reuses no permit id"
    );
    assert_eq!(
        state.log.iter().filter(|entry| *entry == "jwks").count(),
        2,
        "verification material is re-read under the new generation"
    );
    assert_eq!(
        state.log.iter().filter(|entry| *entry == "bind").count(),
        2,
        "the binding is re-established under the new generation"
    );
    assert_eq!(
        state.tokens("user_catalog"),
        2,
        "and so is the catalog admission"
    );
    drop(state);
    harness.wait_snapshot(|snapshot| snapshot.account_generation == 2);
}

#[test]
fn cancelling_fences_the_live_context_and_returns_to_idle() {
    let harness = Harness::start(one_hosted_pair());
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 2);
    let live = lock(&harness.world).permits();

    harness.control.cancel();
    harness.wait_for(|world| !world.fenced.is_empty());
    assert_eq!(
        lock(&harness.world).fenced[0]
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        live.iter()
            .map(|permit| permit.permit_id)
            .collect::<BTreeSet<_>>()
    );
    wait_until(|| harness.control.snapshot().stage == Stage::AwaitingAuthority);
    assert!(harness.control.snapshot().records.is_empty());
}

/// **Blob access is refreshed before each dataset's sessions.** One push per
/// pass was not enough: the sessions run one after another, and on a phone
/// with a dozen workspaces the later ones outlived the token's grace, so the
/// file pass each session triggers was refused while its documents had just
/// arrived.
#[test]
fn blob_access_is_pushed_once_per_pass_and_again_before_each_dataset() {
    let harness = Harness::start(hosted([1, 2, 3]));
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 6);
    let state = lock(&harness.world);
    assert_eq!(
        state.access_pushes,
        1 + 6,
        "one for the pass, one per dataset session"
    );
}

#[test]
fn all_workspaces_keep_subscriptions_across_heartbeat_beyond_sixteen() {
    let world = World::shared();
    {
        let mut state = lock(&world);
        let candidates: Vec<_> = (1..=20).map(|i| candidate_v2(i, Some(i))).collect();
        let (head, tail) = candidates.split_at(12);
        state.receipts = vec![
            receipt(
                head.to_vec(),
                (1..=20).map(live).collect(),
                CloudCoverageV2::Complete,
                Some(&head[11]),
            ),
            receipt(tail.to_vec(), Vec::new(), CloudCoverageV2::Complete, None),
        ];
        for i in 1..=20 {
            state
                .descriptors_v2
                .insert(workspace(i), descriptor_v2(i, Some(i)));
        }
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 40);
    harness.wait_snapshot(|snapshot| snapshot.records.len() == 40 && snapshot.carried == 0);
    let first = lock(&harness.world).permits();
    assert_eq!(first.len(), 40);
    harness.control.wake();
    harness.tick_until(CHANGE_DEBOUNCE.as_millis() as u64, |world| {
        world.contexts.len() == 2
    });
    harness.wait_for(|world| world.opened.len() == 80);
    let state = lock(&harness.world);
    assert_eq!(state.contexts[1].2.len(), 40);
    assert!(
        state.fenced.is_empty(),
        "healthy subscriptions are retained past sixteen workspaces"
    );
    assert_eq!(
        state
            .permits()
            .iter()
            .map(|p| p.permit_id)
            .collect::<BTreeSet<_>>(),
        first.iter().map(|p| p.permit_id).collect::<BTreeSet<_>>(),
        "refresh reuses unexpired permits"
    );
}

#[test]
fn a_remote_hint_reschedules_the_next_pass_behind_the_debounce() {
    let world = one_hosted_pair();
    lock(&world).remote_hints = 1;
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.records.len() == 2);

    // The hint is trailing: nothing runs until the debounce window closes,
    // and the injected clock only moves when this test moves it.
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(lock(&harness.world).contexts.len(), 1);
    harness.tick_until(CHANGE_DEBOUNCE.as_millis() as u64, |world| {
        world.contexts.len() == 2
    });
    harness.wait_for(|world| world.opened.len() == 4);
}

#[test]
fn a_failed_pass_backs_off_and_the_delay_doubles() {
    let world = one_hosted_pair();
    lock(&world).discover_failures = 2;
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    wait_until(|| harness.control.snapshot().failure_code == Some(CLOUD_OFFLINE));
    assert_eq!(harness.control.snapshot().retry_at_millis, Some(2_000));
    assert!(
        lock(&harness.world).contexts.is_empty(),
        "a pass without a catalog replaces no context"
    );

    harness.tick(1_000);
    wait_until(|| harness.control.snapshot().retry_at_millis == Some(4_000));
    harness.tick(2_000);
    harness.wait_for(|world| world.opened.len() == 2);
    harness.wait_snapshot(|snapshot| snapshot.failure_code.is_none());
    assert_eq!(harness.control.snapshot().retry_at_millis, None);
}

#[test]
fn a_same_session_refresh_keeps_the_generation_and_the_live_context() {
    let harness = Harness::start(one_hosted_pair());
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.records.len() == 2);
    let live = lock(&harness.world).permits();

    let previous = authority("1");
    let mut next = authority("1");
    next.record_version = 4;
    next.access = AccessToken("access-refreshed".into());
    assert!(harness.control.refresh_authorization(previous, next));
    std::thread::sleep(Duration::from_millis(30));

    let state = lock(&harness.world);
    assert_eq!(state.contexts.len(), 1, "a refresh is not a new context");
    assert!(state.fenced.is_empty(), "a refresh fences nothing");
    drop(state);
    assert_eq!(harness.control.snapshot().account_generation, 1);
    assert_eq!(
        harness
            .control
            .snapshot()
            .records
            .iter()
            .map(|record| record.permit_id)
            .collect::<BTreeSet<_>>(),
        live.iter()
            .map(|permit| permit.permit_id)
            .collect::<BTreeSet<_>>()
    );

    // A different session is an account switch, not a refresh.
    assert!(
        !harness
            .control
            .refresh_authorization(authority("1"), authority("2"))
    );
}

#[test]
fn expired_worker_session_reissues_and_fences_the_old_permits() {
    let world = one_hosted_pair();
    lock(&world).expired_opens = 1;
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 2);
    let state = lock(&harness.world);
    assert_eq!(state.contexts.len(), 2);
    let old: BTreeSet<_> = state.contexts[0].2.iter().map(|p| p.permit_id).collect();
    let renewed: BTreeSet<_> = state.contexts[1].2.iter().map(|p| p.permit_id).collect();
    assert!(old.is_disjoint(&renewed));
    assert_eq!(renewed.len(), 1, "only the expired scope is re-admitted");
    assert!(old.contains(&state.fenced[0][0]));
}

#[test]
fn repeated_worker_expiry_has_a_bounded_retry_then_backoff() {
    let world = one_hosted_pair();
    lock(&world).expired_opens = 100;
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.failure_code == Some(CREDENTIAL_EXPIRED));
    let snapshot = harness.control.snapshot();
    assert!(snapshot.retry_at_millis.is_some());
    let state = lock(&harness.world);
    assert_eq!(state.expired_opens, 96, "two attempts per dataset");
    assert!(state.opened.is_empty());
}

#[test]
fn renewed_account_access_resumes_a_pass_parked_after_401() {
    let world = one_hosted_pair();
    lock(&world).discover_auth_failures = 1;
    let harness = Harness::start(world);
    let previous = authority("1");
    harness.control.update_authorization(previous.clone());
    harness.wait_snapshot(|snapshot| snapshot.failure_code == Some(SESSION_EXPIRED));
    let mut next = previous.clone();
    next.access = AccessToken("renewed-access".into());
    harness.control.refresh_authorization(previous, next);
    harness.wait_for(|world| world.opened.len() == 2);
    assert_eq!(harness.control.snapshot().account_generation, 1);
}

#[test]
fn heartbeats_cannot_fill_the_command_queue_and_block_refresh() {
    let world = World::shared();
    let mut parked_worker = None;
    let control = Control::start_with_spawn(
        FakeApi(world.clone()),
        FakeHost(world),
        PauseClock::new(),
        Arc::new(Steps::default()),
        true,
        |_| {},
        |worker| {
            // Keep the receiver alive without consuming any queued commands.
            parked_worker = Some(worker);
            Ok(())
        },
    );
    for _ in 0..100 {
        assert!(control.wake());
    }
    let previous = authority("1");
    let mut next = previous.clone();
    next.access = AccessToken("renewed-access".into());
    assert!(control.refresh_authorization(previous, next));
    drop(parked_worker);
}

#[test]
fn a_backgrounded_host_starts_no_pass_and_resumes_where_it_stopped() {
    let harness = Harness::start(one_hosted_pair());
    harness.control.set_foreground(false);
    harness.control.update_authorization(authority("1"));
    std::thread::sleep(Duration::from_millis(30));
    assert!(harness.log().is_empty(), "no effect runs in the background");

    harness.control.set_foreground(true);
    harness.wait_for(|world| world.opened.len() == 2);
}

#[test]
fn every_stage_keeps_its_control_protocol_spelling() {
    assert_eq!(
        [
            Stage::AwaitingAuthority,
            Stage::Connecting,
            Stage::Reconciling,
            Stage::WaitingForConsent,
            Stage::Offline,
            Stage::Unavailable,
            Stage::CheckingCoverage,
            Stage::Settled,
        ]
        .map(Stage::as_str),
        [
            "awaiting_authority",
            "connecting",
            "reconciling",
            "waiting_for_consent",
            "offline",
            "unavailable",
            "checking_coverage",
            "settled",
        ]
    );
}
