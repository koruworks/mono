// SPDX-License-Identifier: MPL-2.0

//! The whole coordinator, driven through its real worker with in-memory
//! ports. Nothing here touches disk, the network, a Store or telemetry: the
//! `Api` fake answers the five hosted reads from a table and the `Host` fake
//! records the platform effects in the order they were asked for.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use koru_account_wire::cloud_sync::{
    DatasetRouteResultV1, SyncCredential, SyncTokenResponseV1, SyncUnavailableCode,
};
use koru_account_wire::{
    AccessToken, CloudWorkspaceCandidateKindV1, JwkV1, JwksResponseV1, Version1,
};
use koru_device::ReplicationEndpointId;

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

/// One shared table behind both fakes. The coordinator is the only writer of
/// its own state; everything here is an observation of what it asked for.
#[derive(Default)]
struct World {
    log: Vec<String>,
    pages: Vec<CloudWorkspacePageV1>,
    denied: HashSet<WorkspaceId>,
    not_ready: HashSet<WorkspaceId>,
    descriptors: HashMap<WorkspaceId, CloudWorkspaceDescriptorV1>,
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
    contexts: Vec<(u64, u64, Vec<CloudPermitV1>)>,
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
            ..Self::default()
        }))
    }
    fn note(&mut self, entry: impl Into<String>) {
        self.log.push(entry.into());
    }
    fn permits(&self) -> Vec<CloudPermitV1> {
        self.contexts
            .last()
            .map(|(_, _, permits)| permits.clone())
            .unwrap_or_default()
    }
}

struct FakeApi(Arc<Mutex<World>>);
struct FakeHost(Arc<Mutex<World>>);

fn lock(world: &Arc<Mutex<World>>) -> std::sync::MutexGuard<'_, World> {
    world.lock().unwrap_or_else(|error| error.into_inner())
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
        if world
            .cancel_discovery_page
            .as_ref()
            .is_some_and(|(at, _)| *at == index)
        {
            let (_, cancel) = world.cancel_discovery_page.take().unwrap();
            cancel.request();
        }
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
}

impl Host for FakeHost {
    fn local_workspace(
        &self,
        account: KoruIdentityId,
        _profile: ProfileId,
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
        Ok(())
    }

    fn ensure_workspace_presence(
        &self,
        descriptor: &CloudWorkspaceDescriptorV1,
    ) -> Result<(), Failure> {
        let mut world = lock(&self.0);
        world.note(format!(
            "workspace:{}:{}",
            descriptor.workspace_id.to_storage(),
            descriptor.manifest_document_id
        ));
        Ok(())
    }

    fn replace_context(
        &self,
        sequence: u64,
        account: KoruIdentityId,
        account_generation: u64,
        permits: &[CloudPermitV1],
    ) -> Result<(), Failure> {
        let mut world = lock(&self.0);
        world.note(format!("context:{sequence}:{}", permits.len()));
        assert_eq!(account, self::account());
        assert!(permits.len() <= MAX_LIVE_PERMITS);
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
        permit: &CloudPermitV1,
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
        assert_eq!(permit.dataset, dataset);
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
}

fn index_of(workspace: WorkspaceId) -> u8 {
    workspace.as_uuid().as_bytes()[15]
}

#[test]
fn a_blocked_profile_does_not_hold_another_profiles_manifest() {
    let world = World::shared();
    let (release, wait) = mpsc::channel();
    {
        let mut state = lock(&world);
        state.pages = vec![page(vec![candidate(1), candidate(2)], None)];
        for index in [1, 2] {
            state
                .descriptors
                .insert(workspace(index), descriptor(index));
        }
        state
            .blocked
            .insert(workspace(1), Arc::new(Mutex::new(wait)));
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.contains(&profile_dataset(2)));
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
                let mut row = candidate(index);
                row.profile_id = profile(if index == 1 { 1 } else { 2 });
                let mut description = descriptor(index);
                description.profile_id = row.profile_id;
                state.descriptors.insert(workspace(index), description);
                row
            })
            .collect();
        state.pages = vec![page(candidates, None)];
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
    harness.wait_for(|world| world.opened.len() == 40);
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

fn one_hosted_pair() -> Arc<Mutex<World>> {
    let world = World::shared();
    {
        let mut state = lock(&world);
        state.pages = vec![page(vec![candidate(1)], None)];
        state.descriptors.insert(workspace(1), descriptor(1));
    }
    world
}

// ----- tests --------------------------------------------------------------

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
        assert_eq!(
            world.log.iter().filter(|s| s.starts_with("token:")).count(),
            1
        );
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
        let mut sibling = candidate(2);
        sibling.profile_id = profile(1);
        let mut description = descriptor(2);
        description.profile_id = profile(1);
        state.descriptors.insert(workspace(2), description);
        state.pages[0].workspaces.push(sibling);
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
    harness.wait_for(|w| w.opened.len() == 4);
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
fn a_fresh_device_selects_describes_bootstraps_and_opens_workspace_then_profile() {
    let harness = Harness::start(one_hosted_pair());
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == 2);

    let log = harness.log();
    let expected = [
        "discover".to_owned(),
        "jwks".to_owned(),
        format!("select:{}", workspace(1).to_storage()),
        format!("token:{}", workspace(1).to_storage()),
        format!("local:{}", workspace(1).to_storage()),
        format!("describe:{}", workspace(1).to_storage()),
        format!("profile:{}:{}", profile(1).to_storage(), account()),
        format!("workspace:{}:{}", workspace(1).to_storage(), manifest(1)),
        "context:1:2".to_owned(),
        "open:workspace".to_owned(),
        "open:profile".to_owned(),
    ];
    assert_eq!(log, expected);

    let world = lock(&harness.world);
    assert!(world.fenced.is_empty(), "nothing was live to fence");
    assert_eq!(
        world.opened,
        vec![workspace_dataset(1, replica(1)), profile_dataset(1)]
    );
    let permits = world.permits();
    assert_eq!(permits.len(), 2);
    assert_eq!(permits[0].version, 1);
    assert_eq!(permits[0].dataset, workspace_dataset(1, replica(1)));
    assert_eq!(permits[0].workspace_descriptor, Some(descriptor(1)));
    assert!(permits[0].allowed_profile_units.is_empty());
    assert_eq!(permits[0].expires_at_seconds, 4_000);
    assert_eq!(
        permits[0].authority_workspace_incarnation_id,
        cloud_id(incarnation(1))
    );
    assert_eq!(permits[1].dataset, profile_dataset(1));
    assert_eq!(permits[1].workspace_descriptor, None);
    assert_eq!(
        permits[1].allowed_profile_units,
        vec![
            ProfileUnitRef::Identity {
                profile: profile(1)
            },
            ProfileUnitRef::Private {
                profile: profile(1)
            },
            ProfileUnitRef::Personal {
                owner: profile(1),
                workspace: workspace(1),
            },
        ]
    );
    assert_ne!(permits[0].permit_id, permits[1].permit_id);
    drop(world);

    // Progress snapshots expose both records before their sessions settle.
    harness
        .wait_snapshot(|snapshot| snapshot.records.len() == 2 && snapshot.stage == Stage::Settled);
    let snapshot = harness.control.snapshot();
    assert_eq!(snapshot.version, 1);
    assert_eq!(snapshot.sequence, 1);
    assert_eq!(snapshot.account_id, Some(account()));
    assert_eq!(snapshot.records.len(), 2);
    assert!(snapshot.records.iter().all(|r| r.stage == Stage::Settled));
    assert_eq!(snapshot.stage, Stage::Settled);
    assert_eq!(snapshot.failure_code, None);
    assert!(snapshot.deferred.is_empty());
    // Sorted by permit id, as CloudStatus requires.
    let ids: Vec<_> = snapshot.records.iter().map(|r| r.permit_id).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
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
            && snapshot
                .records
                .iter()
                .any(|record| record.profile_id == profile(1) && record.stage == Stage::Reconciling)
    }));
}

#[test]
fn an_originating_device_uses_its_own_manifest_identity_and_skips_the_hosted_lookup() {
    let world = World::shared();
    {
        let mut state = lock(&world);
        state.pages = vec![page(vec![candidate(1)], None)];
        state.local.insert(
            workspace(1),
            LocalBootstrap {
                profile_id: profile(1),
                workspace_id: workspace(1),
                manifest_document_id: manifest(9),
            },
        );
    }
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
    let descriptor = permits[0].workspace_descriptor.expect("workspace permit");
    assert_eq!(descriptor.manifest_document_id, manifest(9));
    assert_eq!(descriptor.logical_replica_id, replica(1));
    assert_eq!(descriptor.lineage_id, incarnation(1));
    assert_eq!(descriptor.membership_epoch, 5);
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
    assert_eq!(snapshot.deferred[0].workspace_id, workspace(1));
    assert_eq!(snapshot.retry_at_millis, Some(2_000));

    // The retry happens once the backoff is due, and the mapping that has
    // appeared meanwhile is used without any restart.
    lock(&harness.world).not_ready.clear();
    harness.tick(1_000);
    harness.wait_for(|world| world.opened.len() == 2);
    harness.wait_snapshot(|snapshot| snapshot.failure_code.is_none());
}

#[test]
fn a_denied_selection_defers_that_pair_and_admits_the_other() {
    let world = World::shared();
    {
        let mut state = lock(&world);
        state.pages = vec![page(vec![candidate(1), candidate(2)], None)];
        state.descriptors.insert(workspace(1), descriptor(1));
        state.descriptors.insert(workspace(2), descriptor(2));
        state.denied.insert(workspace(1));
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.failure_code == Some(SELECTION_DENIED));

    let log = harness.log();
    assert!(
        !log.contains(&format!("local:{}", workspace(1).to_storage())),
        "a denied pair never reaches the descriptor step: {log:?}"
    );
    let permits = lock(&harness.world).permits();
    assert_eq!(permits.len(), 2);
    assert!(permits.iter().all(|permit| permit.profile_id == profile(2)));
    let snapshot = harness.control.snapshot();
    assert_eq!(snapshot.deferred.len(), 1);
    assert_eq!(snapshot.deferred[0].code, SELECTION_DENIED);
    assert_eq!(snapshot.failure_code, Some(SELECTION_DENIED));
}

#[test]
fn a_credential_naming_another_replica_is_refused_after_one_reissue() {
    let world = one_hosted_pair();
    lock(&world).issued_replica.insert(workspace(1), replica(8));
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_snapshot(|snapshot| snapshot.failure_code == Some(DATASET_MISMATCH));

    let state = lock(&harness.world);
    assert_eq!(
        state
            .log
            .iter()
            .filter(|entry| entry.starts_with("token:"))
            .count(),
        2,
        "exactly one bounded reissue: {:?}",
        state.log
    );
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
        lock(&harness.world).fenced[0],
        live.iter()
            .map(|permit| permit.permit_id)
            .collect::<Vec<_>>()
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
    let world = World::shared();
    {
        let mut state = lock(&world);
        let candidates: Vec<_> = (1..=3).map(candidate).collect();
        state.pages = vec![page(candidates.clone(), None)];
        for candidate in &candidates {
            state.descriptors.insert(
                candidate.workspace_id,
                descriptor(index_of(candidate.workspace_id)),
            );
        }
    }
    let harness = Harness::start(world);
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
    let candidates: Vec<_> = (1..=20).map(candidate).collect();
    {
        let mut state = lock(&world);
        let (head, tail) = candidates.split_at(12);
        state.pages = vec![
            page(head.to_vec(), Some(&head[11])),
            page(tail.to_vec(), None),
        ];
        for candidate in &candidates {
            state.descriptors.insert(
                candidate.workspace_id,
                descriptor(index_of(candidate.workspace_id)),
            );
        }
    }
    let harness = Harness::start(world);
    harness.control.update_authorization(authority("1"));
    harness.wait_for(|world| world.opened.len() == candidates.len() * 2);
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
    assert_eq!(state.permits(), first, "refresh reuses unexpired permits");
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
            .collect::<Vec<_>>(),
        live.iter()
            .map(|permit| permit.permit_id)
            .collect::<Vec<_>>()
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
    assert_eq!(
        state.fenced[0].iter().copied().collect::<BTreeSet<_>>(),
        old
    );
    assert_eq!(
        state
            .log
            .iter()
            .filter(|line| line.starts_with("token:"))
            .count(),
        2
    );
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
