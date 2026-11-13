// SPDX-License-Identifier: MPL-2.0

use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use crate::account::cloud_sync::{
    CLOUD_SYNC_UNAVAILABLE, CloudSyncPort, DeviceCloudSync, STATE_VERSION,
};
use koru_account_core::WorkspaceId;
use koru_account_wire::{
    BeginWorkspaceCloudActivationRequestV1, CloudWorkspaceCandidateKindV1,
    CloudWorkspaceCandidateV1, CloudWorkspacePageV1, CompleteWorkspaceCloudActivationRequestV1,
    MutateWorkspaceCloudRequestV1, WorkspaceCloudActivationChallengeV1,
    WorkspaceCloudLifecycleResponseV1,
};
use koru_agent_wire::CloudSyncProfile;
use serde::Serialize;

/// The incarnation a Workspace got when some *other* device activated it.
const ELSEWHERE_INCARNATION: u128 = 0xe15e;

// ----- the host port, in memory -----------------------------------------

#[derive(Default)]
struct FakePort {
    workspaces: Mutex<Vec<(ProfileId, WorkspaceId)>>,
    state: Mutex<DeviceCloudSync>,
    stores: AtomicUsize,
    /// Whether this host starts a created Profile's Cloud sync on — the
    /// desktop does, and a host that cannot open its Store does not.
    starts_on: AtomicBool,
    /// The Store cannot be reached, so it cannot be opened to Cloud.
    closed: AtomicBool,
    /// Every Profile the Store was opened to Cloud for, in order.
    opened: Mutex<Vec<ProfileId>>,
}

impl FakePort {
    fn with(workspaces: Vec<(ProfileId, WorkspaceId)>) -> Arc<Self> {
        Arc::new(Self {
            workspaces: Mutex::new(workspaces),
            ..Default::default()
        })
    }
    fn opened(&self) -> Vec<ProfileId> {
        self.opened.lock().unwrap().clone()
    }
    fn saved(&self) -> DeviceCloudSync {
        self.state.lock().unwrap().clone()
    }
    /// A Workspace created after the switch was already answered.
    fn created(&self, profile: ProfileId, workspace: WorkspaceId) {
        self.workspaces.lock().unwrap().push((profile, workspace));
    }
}

impl CloudSyncPort for FakePort {
    fn workspaces(&self) -> Result<Vec<(ProfileId, WorkspaceId)>, ServiceError> {
        Ok(self.workspaces.lock().unwrap().clone())
    }
    fn load(&self) -> Result<DeviceCloudSync, ServiceError> {
        Ok(self.state.lock().unwrap().clone())
    }
    fn store(&self, state: &DeviceCloudSync) -> Result<(), ServiceError> {
        self.stores.fetch_add(1, Ordering::AcqRel);
        *self.state.lock().unwrap() = state.clone();
        Ok(())
    }
    fn starts_created_profiles_on(&self) -> bool {
        self.starts_on.load(Ordering::Acquire)
    }
    fn open_to_cloud(&self, profile: ProfileId) -> Result<(), ServiceError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ServiceError::retryable("store_unavailable"));
        }
        self.opened.lock().unwrap().push(profile);
        Ok(())
    }
}

// ----- the hosted account, in memory ------------------------------------

/// A hosted Workspace Cloud row, obeying the parts of
/// `workspace-cloud-lifecycle-v1` this handler depends on: a version that only
/// moves forward, an incarnation minted by the first activation, and a refusal
/// when the caller's expected version is not the current one.
#[derive(Clone, Copy)]
struct Row {
    enabled: bool,
    version: u64,
    incarnation: uuid::Uuid,
}

#[derive(Default)]
struct HostedAccount {
    profile: Option<ProfileId>,
    rows: Mutex<BTreeMap<WorkspaceId, Row>>,
    /// Activation is refused outright for these, with a code a person's other
    /// Workspaces must survive.
    unauthorized: BTreeSet<WorkspaceId>,
    challenges: Mutex<BTreeMap<uuid::Uuid, WorkspaceId>>,
    paths: Mutex<Vec<String>>,
    minted: AtomicUsize,
    /// Whether the account has Koru Cloud.
    subscribed: AtomicBool,
    complimentary: AtomicBool,
}

impl HostedAccount {
    fn calls(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }
    /// Hosted mutations: begin, complete and disable. Reads are not these.
    fn lifecycle_calls(&self) -> usize {
        self.calls()
            .iter()
            .filter(|path| {
                path.ends_with("/cloud/activation")
                    || path.ends_with("/cloud/activation/complete")
                    || path.ends_with("/cloud/disable")
            })
            .count()
    }
    /// Reads of the account's subscription.
    fn subscription_reads(&self) -> usize {
        self.calls()
            .iter()
            .filter(|path| path.starts_with("/v1/account/subscription"))
            .count()
    }
    /// Reads of one Workspace's lifecycle row.
    fn reads(&self) -> usize {
        self.calls()
            .iter()
            .filter(|path| {
                path.starts_with("/v1/account/workspaces/") && path.ends_with("/cloud/state")
            })
            .count()
    }
    /// Another device turned this Workspace off: the row stays, its version
    /// moved, and the account's enabled list no longer names it.
    fn disabled_elsewhere(&self, workspace: WorkspaceId) {
        let mut rows = self.rows.lock().unwrap();
        let row = rows.get_mut(&workspace).expect("a row to disable");
        row.enabled = false;
        row.version += 1;
    }
    fn row(&self, workspace: WorkspaceId) -> Option<Row> {
        self.rows.lock().unwrap().get(&workspace).copied()
    }
    /// Another device turned this Workspace on, so the local device's guess of
    /// version 0 is stale and the account already lists it as enabled.
    fn enabled_elsewhere(&self, workspace: WorkspaceId) {
        self.rows.lock().unwrap().insert(
            workspace,
            Row {
                enabled: true,
                version: 4,
                incarnation: uuid::Uuid::from_u128(ELSEWHERE_INCARNATION),
            },
        );
    }
    fn mint(&self) -> uuid::Uuid {
        uuid::Uuid::from_u128(1 + self.minted.fetch_add(1, Ordering::AcqRel) as u128)
    }
    fn workspace_of(path: &str) -> WorkspaceId {
        let id = path
            .trim_start_matches("/v1/account/workspaces/")
            .split('/')
            .next()
            .unwrap();
        WorkspaceId::parse(id).unwrap()
    }
    fn json<T: Serialize>(value: &T) -> Result<HttpResponse, TransportError> {
        Ok(HttpResponse {
            status: 200,
            body: serde_json::to_vec(value).unwrap(),
        })
    }
    fn refuse(status: u16, code: &str) -> Result<HttpResponse, TransportError> {
        Ok(HttpResponse {
            status,
            body: serde_json::to_vec(&serde_json::json!({"code": code, "message": "refused"}))
                .unwrap(),
        })
    }
    fn answer(&self, workspace: WorkspaceId, row: Row) -> Result<HttpResponse, TransportError> {
        Self::json(&WorkspaceCloudLifecycleResponseV1 {
            version: Version1,
            status: koru_account_core::workspace_lifecycle::WorkspaceLifecycleStatus::Committed,
            workspace_id: workspace,
            state: if row.enabled {
                koru_account_core::workspace_lifecycle::WorkspaceCloudState::Enabled
            } else {
                koru_account_core::workspace_lifecycle::WorkspaceCloudState::Disabled
            },
            incarnation_id: row.incarnation,
            membership_epoch: row.version,
            cloud_access_epoch: row.version,
            authorization_version: row.version,
        })
    }
}

impl Transport for HostedAccount {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let path = request.path_and_query.clone();
        self.paths.lock().unwrap().push(path.clone());
        if path.starts_with("/v1/account/subscription") {
            let subscriptions = if self.subscribed.load(Ordering::Acquire) {
                serde_json::json!([{"status": "active"}])
            } else {
                serde_json::json!([])
            };
            return Self::json(&serde_json::json!({
                "subscriptions": subscriptions,
                "next_cursor": null,
                "cloud_access": self.subscribed.load(Ordering::Acquire)
                    || self.complimentary.load(Ordering::Acquire),
            }));
        }
        if path == "/v1/account/cloud/workspaces" {
            let rows = self.rows.lock().unwrap();
            return Self::json(&CloudWorkspacePageV1 {
                version: Version1,
                workspaces: rows
                    .iter()
                    .filter(|(_, row)| row.enabled)
                    .map(|(workspace, row)| CloudWorkspaceCandidateV1 {
                        kind: CloudWorkspaceCandidateKindV1::Cloud,
                        profile_id: self.profile.unwrap(),
                        workspace_id: *workspace,
                        workspace_incarnation_id: row.incarnation,
                        authorization_version: row.version as i64,
                        membership_epoch: row.version as i64,
                        cloud_access_epoch: row.version as i64,
                    })
                    .collect(),
                next: None,
            });
        }
        if path.starts_with("/v1/account/workspaces/") && path.ends_with("/cloud/state") {
            use koru_account_core::workspace_lifecycle::{
                WorkspaceCloudState, WorkspaceLifecycleStatus,
            };
            let workspace = Self::workspace_of(&path);
            return match self.row(workspace) {
                Some(row) => Self::json(&WorkspaceCloudLifecycleResponseV1 {
                    version: Version1,
                    status: if row.enabled {
                        WorkspaceLifecycleStatus::AlreadyEnabled
                    } else {
                        WorkspaceLifecycleStatus::AlreadyDisabled
                    },
                    workspace_id: workspace,
                    state: if row.enabled {
                        WorkspaceCloudState::Enabled
                    } else {
                        WorkspaceCloudState::Disabled
                    },
                    incarnation_id: row.incarnation,
                    membership_epoch: row.version,
                    cloud_access_epoch: row.version,
                    authorization_version: row.version,
                }),
                None => Self::json(&WorkspaceCloudLifecycleResponseV1 {
                    version: Version1,
                    status: WorkspaceLifecycleStatus::AlreadyAbsent,
                    workspace_id: workspace,
                    state: WorkspaceCloudState::Absent,
                    incarnation_id: uuid::Uuid::nil(),
                    membership_epoch: 0,
                    cloud_access_epoch: 0,
                    authorization_version: 0,
                }),
            };
        }
        if path.ends_with("/cloud/activation") {
            let workspace = Self::workspace_of(&path);
            let body: BeginWorkspaceCloudActivationRequestV1 =
                serde_json::from_slice(&request.body).unwrap();
            if self.unauthorized.contains(&workspace) {
                return Self::refuse(403, "workspace_not_authorized");
            }
            let current = self.row(workspace).map_or(0, |row| row.version);
            if body.expected_authorization_version != current {
                return Self::refuse(409, "workspace_version_mismatch");
            }
            let challenge_id = self.mint();
            self.challenges
                .lock()
                .unwrap()
                .insert(challenge_id, workspace);
            return Self::json(&WorkspaceCloudActivationChallengeV1 {
                version: Version1,
                challenge_id,
                workspace_id: workspace,
                signing_bytes: workspace.as_uuid().as_bytes().to_vec(),
                expires_at_millis: 9_000_000,
            });
        }
        if path.ends_with("/cloud/activation/complete") {
            let body: CompleteWorkspaceCloudActivationRequestV1 =
                serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body.signature.len(), 64, "the device signature is ed25519");
            let workspace = self.challenges.lock().unwrap()[&body.challenge_id];
            let mut rows = self.rows.lock().unwrap();
            let previous = rows.get(&workspace).copied();
            let row = Row {
                enabled: true,
                version: previous.map_or(1, |row| row.version + 1),
                incarnation: previous.map_or_else(|| self.mint(), |row| row.incarnation),
            };
            rows.insert(workspace, row);
            drop(rows);
            return self.answer(workspace, row);
        }
        if path.ends_with("/cloud/disable") {
            let workspace = Self::workspace_of(&path);
            let body: MutateWorkspaceCloudRequestV1 =
                serde_json::from_slice(&request.body).unwrap();
            let mut rows = self.rows.lock().unwrap();
            let Some(previous) = rows.get(&workspace).copied() else {
                return Self::refuse(409, "workspace_version_mismatch");
            };
            if body.expected_authorization_version != previous.version
                || body.expected_incarnation_id != previous.incarnation
            {
                return Self::refuse(409, "workspace_version_mismatch");
            }
            let row = Row {
                enabled: false,
                version: previous.version + 1,
                ..previous
            };
            rows.insert(workspace, row);
            drop(rows);
            return self.answer(workspace, row);
        }
        panic!("unexpected Cloud sync route {path}");
    }
}

// ----- fixture ----------------------------------------------------------

struct Fixture {
    service: ProductionAccountService,
    hosted: Arc<HostedAccount>,
    port: Arc<FakePort>,
    profile: ProfileId,
}

/// A restored session. Only its presence matters here — the Cloud sync
/// commands read the access token, never the vault secret.
fn restored() -> SessionCredential {
    SessionCredential::new(
        "20000000-0000-4000-8000-000000000002".into(),
        "refresh-vault-only".into(),
    )
    .unwrap()
}

fn fixture(hosted: HostedAccount, workspaces: Vec<(ProfileId, WorkspaceId)>) -> Fixture {
    let profile = hosted.profile.expect("the fixture account owns a Profile");
    let account = KoruIdentityId::mint();
    let hosted = Arc::new(hosted);
    let port = FakePort::with(workspaces);
    let inventory = koru_store_wire::ProfileLifecycleInventory {
        profiles: vec![koru_store_wire::ProfileLifecycleInventoryItem {
            profile_id: profile,
            account_binding: format!("koru:{account}"),
            removed_at_millis: None,
        }],
    };
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(hosted.clone()),
        signed_descriptor(),
        "test".into(),
    )
    .with_local_profile_inventory(Arc::new(StaticInventory(inventory)))
    .with_cloud_sync_port(port.clone());
    service.access = Some(AccessToken("memory-only".into()));
    service.access_expires_at_millis = Some(2_000_000_000_000);
    service.durable_owner = Some(crate::identity::AccountOwner {
        account,
        auth_identity: "30000000-0000-4000-8000-000000000001".parse().unwrap(),
    });
    Fixture {
        service,
        hosted,
        port,
        profile,
    }
}

impl Fixture {
    /// `Err` is the stable code of a `Failed` answer: a refusal of the switch
    /// is answered as one, never published as a change of account stage.
    fn run(&mut self, command: AccountCommand) -> Result<Vec<CloudSyncProfile>, String> {
        match self.service.execute(
            7,
            command,
            Some(&restored()),
            &Cancel(Arc::new(AtomicBool::new(false))),
            &mut |_| {},
        ) {
            Ok(outcome) => {
                assert_eq!(outcome.state.stage, AccountStage::SignedIn);
                match outcome.result {
                    AccountOperationResult::CloudSync { profiles, .. } => Ok(profiles),
                    AccountOperationResult::Failed { code } => Err(code),
                    other => panic!("expected CloudSync or Failed, got {other:?}"),
                }
            }
            Err(error) => {
                panic!("a Cloud sync command must answer, not fail the stage: {error:?}")
            }
        }
    }
    fn set(&mut self, enabled: bool) -> Result<Vec<CloudSyncProfile>, String> {
        let profile_id = self.profile.to_storage();
        self.run(AccountCommand::SetProfileCloudSync {
            request_id: 11,
            profile_id,
            enabled,
        })
    }
    fn inspect(&mut self) -> Result<Vec<CloudSyncProfile>, String> {
        self.run(AccountCommand::InspectCloudSync { request_id: 12 })
    }
}

fn workspaces(profile: ProfileId, count: usize) -> Vec<(ProfileId, WorkspaceId)> {
    (0..count)
        .map(|index| {
            (
                profile,
                WorkspaceId::from_uuid(uuid::Uuid::from_u128(0xf000 + index as u128)),
            )
        })
        .collect()
}

// ----- the tests --------------------------------------------------------

#[test]
fn local_cloud_choices_are_account_scoped_before_session_refresh() {
    let profile = ProfileId::new();
    let mut fixture = fixture(
        HostedAccount {
            profile: Some(profile),
            ..Default::default()
        },
        Vec::new(),
    );
    let account = fixture.service.durable_owner.unwrap().account;
    fixture
        .port
        .state
        .lock()
        .unwrap()
        .accounts
        .entry(account.to_string())
        .or_default()
        .profiles
        .entry(profile)
        .or_default()
        .enabled = true;
    fixture.service.access = None;
    fixture.service.durable_owner = None;
    let choices = fixture.service.cloud_profiles(account);
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].profile_id, profile.to_string());
    assert!(
        fixture
            .service
            .cloud_profiles(KoruIdentityId::mint())
            .is_empty()
    );
    assert!(
        fixture.hosted.calls().is_empty(),
        "local policy must precede HTTP and credentials"
    );
}

#[test]
fn a_host_without_the_port_refuses_rather_than_reporting_an_unapplied_choice() {
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(HostedAccount::default())),
        signed_descriptor(),
        "test".into(),
    );
    service.access = Some(AccessToken("memory-only".into()));
    service.access_expires_at_millis = Some(2_000_000_000_000);
    for command in [
        AccountCommand::InspectCloudSync { request_id: 1 },
        AccountCommand::SetProfileCloudSync {
            request_id: 2,
            profile_id: ProfileId::new().to_storage(),
            enabled: true,
        },
    ] {
        let outcome = service
            .execute(
                1,
                command.clone(),
                Some(&restored()),
                &Cancel(Arc::new(AtomicBool::new(false))),
                &mut |_| {},
            )
            .expect("a refusal of the switch is an answer, not a stage change");
        // Nothing is known, so the answer is a visible failure rather than an
        // empty Profile list that would read as "every Profile is off" - and
        // the person stays signed in.
        assert_eq!(outcome.state.stage, AccountStage::SignedIn);
        assert_eq!(
            outcome.result,
            AccountOperationResult::Failed {
                code: CLOUD_SYNC_UNAVAILABLE.into()
            }
        );
        assert!(matches!(
            command_failure(&command, CLOUD_SYNC_UNAVAILABLE),
            AccountOperationResult::Failed { .. }
        ));
    }
}

#[test]
fn turning_a_profile_on_then_off_carries_every_workspace_with_it() {
    let profile = ProfileId::new();
    let local = workspaces(profile, 3);
    let mut fixture = fixture(
        HostedAccount {
            profile: Some(profile),
            ..Default::default()
        },
        local.clone(),
    );

    let rows = fixture.set(true).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0].enabled,
            rows[0].workspaces,
            rows[0].synced_workspaces
        ),
        (true, 3, 3)
    );
    assert_eq!(rows[0].error, None);
    for (_, workspace) in &local {
        assert!(fixture.hosted.row(*workspace).unwrap().enabled);
    }
    let saved = fixture.port.saved();
    assert_eq!(saved.version, STATE_VERSION);
    let remembered = &saved.accounts.values().next().unwrap().profiles[&profile];
    assert!(remembered.enabled);
    assert_eq!(remembered.workspaces.len(), 3);
    assert!(
        remembered
            .workspaces
            .values()
            .all(|entry| entry.enabled && entry.incarnation_id.is_some() && entry.error.is_none())
    );

    let rows = fixture.set(false).unwrap();
    assert_eq!(
        (
            rows[0].enabled,
            rows[0].synced_workspaces,
            rows[0].error.clone()
        ),
        (false, 0, None)
    );
    for (_, workspace) in &local {
        let row = fixture.hosted.row(*workspace).unwrap();
        assert!(!row.enabled);
        // Disable bumps the version, and the device records the new one.
        assert_eq!(row.version, 2);
    }
    let saved = fixture.port.saved();
    let remembered = &saved.accounts.values().next().unwrap().profiles[&profile];
    assert!(!remembered.enabled);
    assert!(
        remembered
            .workspaces
            .values()
            .all(|entry| !entry.enabled && entry.authorization_version == 2)
    );
}

#[test]
fn a_version_mismatch_is_resolved_from_the_accounts_own_cloud_list() {
    let profile = ProfileId::new();
    let local = workspaces(profile, 2);
    let mut fixture = fixture(
        HostedAccount {
            profile: Some(profile),
            ..Default::default()
        },
        local.clone(),
    );
    // The second Workspace was turned on by another device, so this one's
    // expected version of 0 is stale.
    fixture.hosted.enabled_elsewhere(local[1].1);

    let rows = fixture.set(true).unwrap();
    assert_eq!(
        (rows[0].synced_workspaces, rows[0].error.clone()),
        (2, None)
    );
    let saved = fixture.port.saved();
    let remembered = &saved.accounts.values().next().unwrap().profiles[&profile];
    let learned = &remembered.workspaces[&local[1].1];
    assert!(learned.enabled);
    // The version and incarnation came from the list, not from an activation
    // this device performed — so a later disable can quote them.
    assert_eq!(learned.authorization_version, 4);
    assert_eq!(
        learned.incarnation_id,
        Some(uuid::Uuid::from_u128(ELSEWHERE_INCARNATION))
    );
    assert!(
        fixture
            .hosted
            .calls()
            .contains(&"/v1/account/cloud/workspaces".to_string())
    );

    // And turning it off disables both, including the one this device never
    // activated itself.
    let rows = fixture.set(false).unwrap();
    assert_eq!(
        (rows[0].synced_workspaces, rows[0].error.clone()),
        (0, None)
    );
    assert!(
        local
            .iter()
            .all(|(_, workspace)| { !fixture.hosted.row(*workspace).unwrap().enabled })
    );
}

#[test]
fn one_refused_workspace_does_not_stop_the_others() {
    let profile = ProfileId::new();
    let local = workspaces(profile, 3);
    let mut fixture = fixture(
        HostedAccount {
            profile: Some(profile),
            unauthorized: BTreeSet::from([local[1].1]),
            ..Default::default()
        },
        local.clone(),
    );

    let rows = fixture.set(true).unwrap();
    assert_eq!(
        (rows[0].workspaces, rows[0].synced_workspaces),
        (3, 2),
        "the refusal is one Workspace's, not the Profile's"
    );
    assert_eq!(rows[0].error.as_deref(), Some("workspace_not_authorized"));
    assert!(fixture.hosted.row(local[0].1).unwrap().enabled);
    assert!(fixture.hosted.row(local[2].1).unwrap().enabled);
    assert!(fixture.hosted.row(local[1].1).is_none());
    // The choice survives the refusal: it is what the reconcile pass replays.
    let saved = fixture.port.saved();
    let remembered = &saved.accounts.values().next().unwrap().profiles[&profile];
    assert!(remembered.enabled);
    assert_eq!(
        remembered.workspaces[&local[1].1].error.as_deref(),
        Some("workspace_not_authorized")
    );
}

#[test]
fn inspect_activates_a_workspace_created_after_the_switch_and_is_free_otherwise() {
    let profile = ProfileId::new();
    let local = workspaces(profile, 1);
    let mut fixture = fixture(
        HostedAccount {
            profile: Some(profile),
            ..Default::default()
        },
        local.clone(),
    );
    assert_eq!(fixture.set(true).unwrap()[0].synced_workspaces, 1);

    // Nothing changed: the reconcile pass reads the account's enabled list
    // (that is how another device's switch is seen), mutates nothing, reads
    // no single row and rewrites no remembered state.
    let before = fixture.hosted.lifecycle_calls();
    let reads = fixture.hosted.reads();
    let writes = fixture.port.stores.load(Ordering::Acquire);
    let rows = fixture.inspect().unwrap();
    assert_eq!((rows[0].enabled, rows[0].synced_workspaces), (true, 1));
    assert_eq!(fixture.hosted.lifecycle_calls(), before);
    assert_eq!(fixture.hosted.reads(), reads);
    assert_eq!(fixture.port.stores.load(Ordering::Acquire), writes);

    // A Workspace created afterwards reaches the Cloud with nobody touching
    // the switch again.
    let fresh = WorkspaceId::new();
    fixture.port.created(profile, fresh);
    let rows = fixture.inspect().unwrap();
    assert_eq!((rows[0].workspaces, rows[0].synced_workspaces), (2, 2));
    assert!(fixture.hosted.row(fresh).unwrap().enabled);
}

#[test]
fn a_profile_outside_the_signed_in_account_is_refused_before_any_hosted_call() {
    let profile = ProfileId::new();
    let mut fixture = fixture(
        HostedAccount {
            profile: Some(profile),
            ..Default::default()
        },
        workspaces(profile, 1),
    );
    let code = fixture
        .run(AccountCommand::SetProfileCloudSync {
            request_id: 3,
            profile_id: ProfileId::new().to_storage(),
            enabled: true,
        })
        .unwrap_err();
    assert_eq!(code, "profile_not_authorized");
    assert!(fixture.hosted.calls().is_empty());
    assert_eq!(fixture.port.stores.load(Ordering::Acquire), 0);
}

#[test]
fn a_state_file_from_an_unknown_version_is_discarded_rather_than_half_read() {
    let state = DeviceCloudSync {
        version: STATE_VERSION + 7,
        ..Default::default()
    };
    assert_eq!(state.accepted(), DeviceCloudSync::default());
    let current = DeviceCloudSync::default();
    assert_eq!(current.clone().accepted(), current);
}

#[test]
fn the_reconcile_pass_mirrors_what_another_device_turned_on_and_never_turns_it_off() {
    let profile = ProfileId::new();
    let local = workspaces(profile, 1);
    let mut fixture = fixture(
        HostedAccount {
            profile: Some(profile),
            ..Default::default()
        },
        local.clone(),
    );
    // This device has no remembered choice; another device switched the
    // Profile on. Before the fix, "no choice" read as "off" and the timer
    // disabled the other device's work within one heartbeat.
    fixture.hosted.enabled_elsewhere(local[0].1);

    let rows = fixture.inspect().unwrap();
    assert_eq!(
        (
            rows[0].enabled,
            rows[0].synced_workspaces,
            rows[0].error.clone()
        ),
        (true, 1, None),
        "the switch shows the hosted truth"
    );
    assert!(fixture.hosted.row(local[0].1).unwrap().enabled);
    assert_eq!(
        fixture.hosted.lifecycle_calls(),
        0,
        "the reconcile pass mutates nothing"
    );
    let saved = fixture.port.saved();
    let remembered = &saved.accounts.values().next().unwrap().profiles[&profile];
    assert!(remembered.enabled);
    assert_eq!(remembered.workspaces[&local[0].1].authorization_version, 4);
}

#[test]
fn a_workspace_another_device_turned_off_shows_off_and_can_be_turned_on_again() {
    let profile = ProfileId::new();
    let local = workspaces(profile, 1);
    let mut fixture = fixture(
        HostedAccount {
            profile: Some(profile),
            ..Default::default()
        },
        local.clone(),
    );
    assert_eq!(fixture.set(true).unwrap()[0].synced_workspaces, 1);
    fixture.hosted.disabled_elsewhere(local[0].1);

    // The reconcile pass notices (the list no longer names the Workspace, so
    // its row is read), mirrors "off", and does not fight the other device by
    // re-activating.
    let mutations = fixture.hosted.lifecycle_calls();
    let reads = fixture.hosted.reads();
    let rows = fixture.inspect().unwrap();
    assert_eq!(
        (
            rows[0].enabled,
            rows[0].synced_workspaces,
            rows[0].error.clone()
        ),
        (false, 0, None)
    );
    assert_eq!(fixture.hosted.lifecycle_calls(), mutations);
    assert_eq!(fixture.hosted.reads(), reads + 1);
    assert!(!fixture.hosted.row(local[0].1).unwrap().enabled);

    // A person switching it on here quotes the version the row now holds,
    // which only the read could have told this device.
    let rows = fixture.set(true).unwrap();
    assert_eq!(
        (
            rows[0].enabled,
            rows[0].synced_workspaces,
            rows[0].error.clone()
        ),
        (true, 1, None)
    );
    let row = fixture.hosted.row(local[0].1).unwrap();
    assert!(row.enabled);
    assert_eq!(row.version, 3);
}

// ----- a Profile created with the account (ADR-000-createdwithaccount) ----

/// A desktop: it can open its Store to Cloud, so a created Profile starts on.
fn created_with_the_account(hosted: HostedAccount, subscribed: bool, count: usize) -> Fixture {
    let profile = hosted.profile.expect("the fixture account owns a Profile");
    hosted.subscribed.store(subscribed, Ordering::Release);
    let fixture = fixture(hosted, workspaces(profile, count));
    fixture.port.starts_on.store(true, Ordering::Release);
    let account = fixture.service.durable_owner.unwrap().account;
    fixture.service.cloud_sync_starts_on(account, profile);
    fixture
}

fn a_new_account_profile() -> HostedAccount {
    HostedAccount {
        profile: Some(ProfileId::new()),
        ..Default::default()
    }
}

fn remembered(fixture: &Fixture) -> crate::account::cloud_sync::ProfileCloudSync {
    fixture
        .port
        .saved()
        .accounts
        .values()
        .next()
        .expect("the account has remembered state")
        .profiles[&fixture.profile]
        .clone()
}

/// **Nobody presses the switch.** A Profile created with a Koru Cloud account
/// syncs on the first pass: the Store is opened to Cloud, every Workspace is
/// activated, and the row answers as a pressed switch would.
#[test]
fn a_profile_created_with_the_account_starts_syncing_by_itself() {
    let mut fixture = created_with_the_account(a_new_account_profile(), true, 2);
    assert!(remembered(&fixture).starts_on);
    assert!(
        !remembered(&fixture).enabled,
        "nothing is on before the pass"
    );
    assert_eq!(fixture.hosted.lifecycle_calls(), 0);

    let rows = fixture.inspect().unwrap();
    assert_eq!(
        (
            rows[0].enabled,
            rows[0].workspaces,
            rows[0].synced_workspaces,
            rows[0].error.clone()
        ),
        (true, 2, 2, None)
    );
    assert_eq!(fixture.port.opened(), vec![fixture.profile]);
    let saved = remembered(&fixture);
    assert!(saved.enabled);
    assert!(!saved.starts_on, "the intention is spent once it is done");

    // And it is an ordinary switched-on Profile from here: the next pass
    // opens nothing and activates nothing.
    let mutations = fixture.hosted.lifecycle_calls();
    fixture.inspect().unwrap();
    assert_eq!(fixture.hosted.lifecycle_calls(), mutations);
    assert_eq!(fixture.port.opened().len(), 1);
}

#[test]
fn complimentary_access_starts_a_created_profile_without_a_subscription() {
    let mut fixture = created_with_the_account(a_new_account_profile(), false, 1);
    fixture.hosted.complimentary.store(true, Ordering::Release);
    let outcome = fixture
        .service
        .execute(
            7,
            AccountCommand::InspectCloudSubscription { request_id: 21 },
            Some(&restored()),
            &Cancel(Arc::new(AtomicBool::new(false))),
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(
        outcome.result,
        AccountOperationResult::CloudSubscription {
            request_id: 21,
            current: Some(false),
            access: Some(true),
        }
    );
    let rows = fixture.inspect().unwrap();
    assert_eq!((rows[0].enabled, rows[0].synced_workspaces), (true, 1));
    assert_eq!(fixture.port.opened(), vec![fixture.profile]);
    assert_eq!(fixture.hosted.subscription_reads(), 1);

    // A fresh Agent also uses the grant when no settings page has asked yet.
    let mut fixture = created_with_the_account(a_new_account_profile(), false, 1);
    fixture.hosted.complimentary.store(true, Ordering::Release);
    let rows = fixture.inspect().unwrap();
    assert_eq!((rows[0].enabled, rows[0].synced_workspaces), (true, 1));
}

/// With no Koru Cloud there is nothing to start, and the account is asked
/// once rather than on every heartbeat. The pass after the settings page
/// sees a subscription is the one that starts it.
#[test]
fn it_waits_for_koru_cloud_without_asking_on_every_pass() {
    let mut fixture = created_with_the_account(a_new_account_profile(), false, 1);
    for _ in 0..3 {
        let rows = fixture.inspect().unwrap();
        assert_eq!((rows[0].enabled, rows[0].synced_workspaces), (false, 0));
    }
    assert_eq!(fixture.hosted.subscription_reads(), 1);
    assert_eq!(fixture.hosted.lifecycle_calls(), 0);
    assert!(fixture.port.opened().is_empty());
    assert!(
        remembered(&fixture).starts_on,
        "still waiting, not forgotten"
    );

    // They subscribe, and the Koru Cloud page asks — which is remembered.
    fixture.hosted.subscribed.store(true, Ordering::Release);
    let outcome = fixture
        .service
        .execute(
            7,
            AccountCommand::InspectCloudSubscription { request_id: 21 },
            Some(&restored()),
            &Cancel(Arc::new(AtomicBool::new(false))),
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(
        outcome.result,
        AccountOperationResult::CloudSubscription {
            request_id: 21,
            current: Some(true),
            access: Some(true),
        }
    );
    let rows = fixture.inspect().unwrap();
    assert_eq!((rows[0].enabled, rows[0].synced_workspaces), (true, 1));
    assert_eq!(
        fixture.hosted.subscription_reads(),
        2,
        "the pass read the page's answer, not the account again"
    );
}

/// The Store half comes first. While it cannot be written the hosted half is
/// not started — documents would sync with every file held back — and the
/// pass after the Store answers finishes the job.
#[test]
fn a_store_that_cannot_be_opened_starts_nothing_until_it_can() {
    let mut fixture = created_with_the_account(a_new_account_profile(), true, 1);
    fixture.port.closed.store(true, Ordering::Release);
    let rows = fixture.inspect().unwrap();
    assert_eq!((rows[0].enabled, rows[0].synced_workspaces), (false, 0));
    assert_eq!(fixture.hosted.lifecycle_calls(), 0);
    assert!(remembered(&fixture).starts_on);

    fixture.port.closed.store(false, Ordering::Release);
    let rows = fixture.inspect().unwrap();
    assert_eq!((rows[0].enabled, rows[0].synced_workspaces), (true, 1));
    assert!(!remembered(&fixture).starts_on);
}

/// A Workspace the account refuses does not spend the intention: the others
/// sync, and the refused one is tried again on the next pass.
#[test]
fn a_refused_workspace_is_tried_again_rather_than_given_up() {
    let profile = ProfileId::new();
    let refused = workspaces(profile, 2)[1].1;
    let mut fixture = created_with_the_account(
        HostedAccount {
            profile: Some(profile),
            unauthorized: BTreeSet::from([refused]),
            ..Default::default()
        },
        true,
        2,
    );

    let rows = fixture.inspect().unwrap();
    assert_eq!((rows[0].enabled, rows[0].synced_workspaces), (true, 1));
    assert_eq!(rows[0].error.as_deref(), Some("workspace_not_authorized"));
    let saved = remembered(&fixture);
    assert!(saved.enabled && saved.starts_on);
}

/// **A person's answer replaces the default.** Turning the switch off before
/// anything started leaves it off for good, and a creation retried afterwards
/// does not arm it again.
#[test]
fn a_switch_somebody_moved_is_never_overridden_by_the_default() {
    let mut fixture = created_with_the_account(a_new_account_profile(), false, 1);
    let rows = fixture.set(false).unwrap();
    assert!(!rows[0].enabled);
    assert!(!remembered(&fixture).starts_on);

    let account = fixture.service.durable_owner.unwrap().account;
    fixture
        .service
        .cloud_sync_starts_on(account, fixture.profile);
    fixture.hosted.subscribed.store(true, Ordering::Release);
    fixture.service.cloud_access = None;
    let rows = fixture.inspect().unwrap();
    assert_eq!((rows[0].enabled, rows[0].synced_workspaces), (false, 0));
    assert_eq!(fixture.hosted.lifecycle_calls(), 0);
    assert!(fixture.port.opened().is_empty());
}

/// A host that cannot open its Store — the phone today — remembers no
/// intention at all, so nothing starts half-way there.
#[test]
fn a_host_that_cannot_open_its_store_starts_nothing() {
    let hosted = a_new_account_profile();
    let profile = hosted.profile.unwrap();
    hosted.subscribed.store(true, Ordering::Release);
    let mut fixture = fixture(hosted, workspaces(profile, 1));
    let account = fixture.service.durable_owner.unwrap().account;
    fixture.service.cloud_sync_starts_on(account, profile);
    assert_eq!(fixture.port.stores.load(Ordering::Acquire), 0);

    let rows = fixture.inspect().unwrap();
    assert_eq!((rows[0].enabled, rows[0].synced_workspaces), (false, 0));
    assert_eq!(fixture.hosted.subscription_reads(), 0);
    assert_eq!(fixture.hosted.lifecycle_calls(), 0);
}

/// **A Profile chosen from the account has nothing here until the Store is
/// open** — that was the deadlock: every pull refused `mode_refuses_transport`
/// while the Store waited for a Workspace before it would open. It opens on
/// the first pass with no Workspace at all, once, and the switch reads on.
/// The Workspace that then arrives was already syncing, so nothing is
/// activated and the intention is spent.
#[test]
fn a_profile_chosen_from_the_account_opens_before_anything_has_arrived() {
    let mut fixture = created_with_the_account(a_new_account_profile(), true, 0);
    for _ in 0..3 {
        let rows = fixture.inspect().unwrap();
        assert_eq!((rows[0].enabled, rows[0].workspaces), (true, 0));
    }
    assert_eq!(
        fixture.port.opened(),
        vec![fixture.profile],
        "opened once, not on every pass"
    );
    assert!(remembered(&fixture).starts_on, "nothing has arrived yet");

    let arrived = WorkspaceId::new();
    fixture.hosted.enabled_elsewhere(arrived);
    fixture.port.created(fixture.profile, arrived);
    let rows = fixture.inspect().unwrap();
    assert_eq!((rows[0].enabled, rows[0].synced_workspaces), (true, 1));
    assert_eq!(
        fixture.hosted.lifecycle_calls(),
        0,
        "it was already syncing"
    );
    assert!(!remembered(&fixture).starts_on);
    assert_eq!(fixture.port.opened().len(), 1);
}

/// **A default never undoes somebody's "off".** A Workspace another device
/// turned off stays off when its Profile starts by itself here; a pressed
/// switch is the only thing that turns it back on.
#[test]
fn a_workspace_turned_off_elsewhere_is_not_turned_on_by_the_default() {
    let mut fixture = created_with_the_account(a_new_account_profile(), true, 0);
    let off = WorkspaceId::new();
    fixture.hosted.enabled_elsewhere(off);
    fixture.hosted.disabled_elsewhere(off);
    fixture.port.created(fixture.profile, off);

    let rows = fixture.inspect().unwrap();
    assert_eq!(
        (rows[0].synced_workspaces, rows[0].error.clone()),
        (0, None)
    );
    assert_eq!(fixture.hosted.lifecycle_calls(), 0);
    assert!(!fixture.hosted.row(off).unwrap().enabled);

    // The person's own switch still can.
    let rows = fixture.set(true).unwrap();
    assert_eq!((rows[0].enabled, rows[0].synced_workspaces), (true, 1));
}

/// **Cloud sync on means the Store is open to Cloud.** The switch opens it
/// in the same command — there is no second answer to give — and once is
/// enough.
#[test]
fn the_switch_opens_the_store_itself() {
    let profile = ProfileId::new();
    let mut fixture = fixture(
        HostedAccount {
            profile: Some(profile),
            ..Default::default()
        },
        workspaces(profile, 1),
    );
    fixture.port.starts_on.store(true, Ordering::Release);
    let rows = fixture.set(true).unwrap();
    assert_eq!((rows[0].enabled, rows[0].synced_workspaces), (true, 1));
    assert_eq!(fixture.port.opened(), vec![profile]);
    fixture.inspect().unwrap();
    fixture.inspect().unwrap();
    assert_eq!(fixture.port.opened().len(), 1, "not on every heartbeat");
}

/// A Profile left switched on with its Store never opened — every install
/// that turned the switch on and did not answer the old question — is opened
/// by the first pass, with no Workspace here and nothing pressed.
#[test]
fn a_profile_already_on_is_opened_by_the_next_pass() {
    let profile = ProfileId::new();
    let mut fixture = fixture(
        HostedAccount {
            profile: Some(profile),
            ..Default::default()
        },
        Vec::new(),
    );
    fixture.port.starts_on.store(true, Ordering::Release);
    let account = fixture.service.durable_owner.unwrap().account;
    fixture
        .port
        .state
        .lock()
        .unwrap()
        .accounts
        .entry(account.to_string())
        .or_default()
        .profiles
        .entry(profile)
        .or_default()
        .enabled = true;

    let rows = fixture.inspect().unwrap();
    assert!(rows[0].enabled);
    assert_eq!(fixture.port.opened(), vec![profile]);
    assert_eq!(
        fixture.hosted.subscription_reads(),
        0,
        "it is already on: nothing to decide"
    );
}
