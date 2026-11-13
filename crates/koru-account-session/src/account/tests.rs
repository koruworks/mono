// SPDX-License-Identifier: MPL-2.0

use super::*;
#[cfg(test)]
use koru_device::{DeviceSigningKey, ReplicationEndpointSecret, SignatureBytes};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    atomic::{AtomicI64, AtomicU64, Ordering},
    Arc, Mutex,
};

impl koru_account_core::workspace_lifecycle::WorkspaceCloudLifecycleRepository for Repository {
    fn begin_activation<'a>(
        &'a self,
        _request: koru_account_core::workspace_lifecycle::BeginWorkspaceActivation,
    ) -> PortFuture<
        'a,
        Result<
            koru_account_core::workspace_lifecycle::WorkspaceActivationChallenge,
            koru_account_core::workspace_lifecycle::WorkspaceLifecycleError,
        >,
    > {
        Box::pin(async {
            Err(koru_account_core::workspace_lifecycle::WorkspaceLifecycleError::Unavailable)
        })
    }

    fn complete_activation<'a>(
        &'a self,
        _request: koru_account_core::workspace_lifecycle::CompleteWorkspaceActivation,
    ) -> PortFuture<
        'a,
        Result<
            koru_account_core::workspace_lifecycle::WorkspaceLifecycleOutcome,
            koru_account_core::workspace_lifecycle::WorkspaceLifecycleError,
        >,
    > {
        Box::pin(async {
            Err(koru_account_core::workspace_lifecycle::WorkspaceLifecycleError::Unavailable)
        })
    }

    fn mutate_lifecycle<'a>(
        &'a self,
        _request: koru_account_core::workspace_lifecycle::MutateWorkspaceLifecycle,
    ) -> PortFuture<
        'a,
        Result<
            koru_account_core::workspace_lifecycle::WorkspaceLifecycleOutcome,
            koru_account_core::workspace_lifecycle::WorkspaceLifecycleError,
        >,
    > {
        Box::pin(async {
            Err(koru_account_core::workspace_lifecycle::WorkspaceLifecycleError::Unavailable)
        })
    }

    fn authorization_snapshot<'a>(
        &'a self,
        _principal: koru_account_core::workspace_authorization::Principal,
        _workspace_id: WorkspaceId,
        _now_millis: i64,
    ) -> PortFuture<
        'a,
        Result<
            Option<koru_account_core::workspace_lifecycle::WorkspaceAuthorizationSnapshotV1>,
            koru_account_core::workspace_lifecycle::WorkspaceLifecycleError,
        >,
    > {
        Box::pin(async { Ok(None) })
    }

    fn changes_after<'a>(
        &'a self,
        _workspace_id: WorkspaceId,
        _authorization_version: u64,
        _limit: u32,
    ) -> PortFuture<
        'a,
        Result<
            Vec<koru_account_core::workspace_lifecycle::WorkspaceCloudChange>,
            koru_account_core::workspace_lifecycle::WorkspaceLifecycleError,
        >,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }
}

#[cfg(test)]
fn registration() -> DeviceRegistration {
    let signing = DeviceSigningKey::from_secret_bytes([9; 32]);
    let endpoint = ReplicationEndpointSecret::from_secret_bytes([7; 32]);
    DeviceRegistration {
        descriptor: DeviceDescriptorV1::new(
            DeviceId::from_uuid(uuid::Uuid::from_u128(1)),
            "Laptop discovery label".into(),
            &signing,
            endpoint.endpoint_id(),
            42,
        ),
        platform: DevicePlatform::Windows,
        app_version: "0.0.3".into(),
        enrollment_protocol_version: None,
        peer_protocol_version: None,
        ownership_proof: None,
    }
}

#[test]
fn signed_device_descriptor_is_required_before_repository_io() {
    assert_eq!(validate_registration(&registration()), Ok(()));
    let mut forged = registration();
    forged.descriptor.endpoint_binding_signature = SignatureBytes::from_bytes([0; 64]);
    assert_eq!(
        validate_registration(&forged),
        Err(AccountIntegrationError::InvalidDescriptor)
    );
}

#[test]
fn legacy_in_flight_device_registration_defaults_capabilities_to_none() {
    let mut value = serde_json::to_value(registration()).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("enrollment_protocol_version");
    object.remove("peer_protocol_version");
    let decoded: DeviceRegistration = serde_json::from_value(value).unwrap();
    assert_eq!(decoded.enrollment_protocol_version, None);
    assert_eq!(decoded.peer_protocol_version, None);
}

#[test]
fn hosted_discovery_metadata_is_bounded() {
    assert_eq!(
        validate_profile_metadata(Some("Desk"), Some("en-US")),
        Ok(())
    );
    assert_eq!(
        validate_profile_metadata(Some(&"x".repeat(MAX_PROFILE_NAME_BYTES + 1)), None),
        Err(AccountIntegrationError::InvalidMetadata)
    );
    assert_eq!(
        validate_profile_metadata(None, Some("")),
        Err(AccountIntegrationError::InvalidMetadata)
    );
    let mut device = registration();
    device.app_version = "x".repeat(MAX_APP_VERSION_BYTES + 1);
    assert_eq!(
        validate_registration(&device),
        Err(AccountIntegrationError::InvalidMetadata)
    );
}

#[test]
fn status_serialization_surface_cannot_contain_pii_or_secrets() {
    let snapshot = AccountStatusSnapshot {
        stage: AccountStatusStage::Ready,
        auth_identity_count: 2,
        authorized_profile_count: 3,
        device_count: 4,
        active_device_count: 1,
        failure: None,
    };
    let rendered = format!("{snapshot:?}");
    let serialized = serde_json::to_string(&snapshot).unwrap();
    for forbidden in [
        "person@example.invalid",
        "https://accounts.google.com",
        "provider-subject",
        "Laptop discovery label",
        "refresh",
        "token",
        "secret",
    ] {
        assert!(!rendered.contains(forbidden));
        assert!(!serialized.contains(forbidden));
    }
    assert!(rendered.contains("authorized_profile_count: 3"));
}

#[test]
fn status_counts_fail_closed_on_unrepresentable_size() {
    assert_eq!(bounded_count(7), Ok(7));
    if usize::BITS > u32::BITS {
        assert_eq!(
            bounded_count((u32::MAX as usize) + 1),
            Err(AccountStatusFailureCode::Corrupt)
        );
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FakeCloudWorkspace {
    pub incarnation_id: uuid::Uuid,
    pub authorization_version: i64,
    pub membership_epoch: i64,
    pub cloud_access_epoch: i64,
    pub enabled: bool,
}

#[derive(Clone, Default)]
struct State {
    identities: BTreeMap<KoruIdentityId, KoruIdentity>,
    auth: BTreeMap<AuthIdentityId, AuthIdentity>,
    flows: BTreeMap<AuthFlowId, TemporaryAuthFlow>,
    proof_commits: BTreeMap<AuthFlowId, KeyedDigest>,
    profiles: BTreeMap<ProfileId, ProfileBinding>,
    profile_auth: BTreeSet<(ProfileId, AuthIdentityId)>,
    devices: BTreeMap<(KoruIdentityId, DeviceId), AccountDevice>,
    device_profiles: BTreeMap<(DeviceId, ProfileId), DeviceProfileState>,
    pending_enrollment: BTreeMap<(DeviceId, ProfileId), PendingEnrollment>,
    sessions: BTreeMap<AccountSessionId, AccountSession>,
    email_challenges: BTreeMap<AuthFlowId, EmailChallengeRow>,
    /// Keyed by address digest, which is what the real table's UNIQUE index is.
    email_credentials: BTreeMap<Vec<u8>, koru_account_core::EmailCredential>,
    cloud_workspaces: BTreeMap<WorkspaceId, FakeCloudWorkspace>,
    /// Active memberships; the value says whether all three sync scopes hold.
    cloud_members: BTreeMap<(ProfileId, WorkspaceId), bool>,
    /// Retained manifest mapping per Workspace: (logical replica, manifest id).
    cloud_manifests: BTreeMap<WorkspaceId, (uuid::Uuid, uuid::Uuid)>,
    lose_link_ack_once: bool,
    lose_rebind_ack_once: bool,
    lose_collision_ack_once: bool,
    lose_flow_insert_ack_after: Option<usize>,
    fail_flow_insert_after: Option<usize>,
    lose_creation_ack_once: bool,
    fail_creation_commit_once: bool,
    session_reads: usize,
    owner_reads: usize,
    fail_session_reads: bool,
    fail_owner_reads: bool,
}

impl State {
    fn register_account_device(
        &mut self,
        mut device: AccountDevice,
        proof: Option<VerifiedDeviceOwnership>,
        parent: Option<AuthFlowId>,
        now: UnixMillis,
    ) -> Result<DeviceRegistrationOutcome, RepositoryError> {
        device
            .descriptor
            .verify()
            .map_err(|_| RepositoryError::Conflict)?;
        let key = (device.registration.koru_identity_id, device.device_id());
        let stored = self.devices.get(&key).cloned();
        let physical = self
            .devices
            .values()
            .find(|value| value.device_id() == device.device_id());
        if physical.is_some_and(|value| {
            let mut expected = value.descriptor.clone();
            if proof.is_some() {
                expected.display_name = device.descriptor.display_name.clone();
            }
            expected != device.descriptor
        }) || self.devices.values().any(|value| {
            value.device_id() != device.device_id()
                && (value.descriptor.signing_public_key == device.descriptor.signing_public_key
                    || value.descriptor.endpoint_id == device.descriptor.endpoint_id)
        }) || stored
            .as_ref()
            .is_some_and(|value| value.registration.revoked_at_millis.is_some())
            || (physical.is_some() && stored.is_none() && proof.is_none())
        {
            return Err(RepositoryError::Conflict);
        }
        if let Some(proof) = proof {
            let flow = self
                .flows
                .get_mut(&proof.challenge.flow_id)
                .ok_or(RepositoryError::Conflict)?;
            if !proof.challenge.matches_registration(&device)
                || proof.challenge.parent_flow_id != parent
                || flow.kind != AuthFlowKind::GoogleNative
                || flow.status != AuthFlowStatus::Pending
                || flow.state_digest != proof.state_digest
                || flow.target_identity_id != Some(key.0)
                || flow.expires_at_millis != proof.challenge.expires_at_millis
                || now >= flow.expires_at_millis
            {
                return Err(RepositoryError::Conflict);
            }
            flow.status = AuthFlowStatus::Consumed;
            flow.consumed_at_millis = Some(now);
        }
        if let Some(stored) = &stored {
            device.registration.registered_at_millis = stored.registration.registered_at_millis;
        }
        for physical in self
            .devices
            .values_mut()
            .filter(|row| row.device_id() == device.device_id())
        {
            physical.descriptor.display_name = device.descriptor.display_name.clone();
        }
        self.devices.insert(key, device.clone());
        Ok(if stored.is_some() {
            DeviceRegistrationOutcome::AlreadyRegistered(device)
        } else {
            DeviceRegistrationOutcome::Registered(device)
        })
    }
}

#[derive(Clone)]
struct PendingEnrollment {
    session_id: AccountSessionId,
    token_id: String,
    attestation_digest: [u8; 32],
    expires_at_millis: UnixMillis,
    mutation_digest: Option<[u8; 32]>,
}

#[derive(Default)]
pub struct Repository(Mutex<State>, Mutex<Option<FlowReadHook>>);

type FlowReadHook =
    Arc<dyn Fn(AuthFlowId, Option<TemporaryAuthFlow>) -> PortFuture<'static, ()> + Send + Sync>;

fn lifecycle_from_binding(profile: &ProfileBinding) -> ProfileLifecycle {
    ProfileLifecycle {
        profile_id: profile.profile_id,
        koru_identity_id: profile.koru_identity_id,
        state: profile.lifecycle_state,
        revision: profile.lifecycle_revision,
        operation_id: profile.lifecycle_operation_id,
        lifecycle_at_millis: profile.deleted_at_millis,
    }
}

impl Repository {
    pub fn inspection_reads(&self) -> (usize, usize) {
        let state = self.0.lock().unwrap();
        (state.session_reads, state.owner_reads)
    }
    pub fn fail_inspection_reads(&self, session: bool, owner: bool) {
        let mut state = self.0.lock().unwrap();
        state.fail_session_reads = session;
        state.fail_owner_reads = owner;
    }
    pub fn replace_session_for_test(&self, session: AccountSession) {
        self.0.lock().unwrap().sessions.insert(session.id, session);
    }
    pub fn set_identity_status_for_test(&self, id: KoruIdentityId, status: KoruIdentityStatus) {
        self.0
            .lock()
            .unwrap()
            .identities
            .get_mut(&id)
            .unwrap()
            .status = status;
    }
    pub fn remove_auth_for_test(&self, id: AuthIdentityId) {
        self.0.lock().unwrap().auth.remove(&id);
    }
    /// Runs after a read snapshot, without holding the fake transaction mutex.
    /// Tests can schedule a concurrent commit while retaining the older read.
    pub fn set_flow_read_hook(&self, hook: FlowReadHook) {
        *self.1.lock().unwrap() = Some(hook);
    }
    pub fn lose_next_creation_ack(&self) {
        self.0.lock().unwrap().lose_creation_ack_once = true;
    }
    pub fn fail_next_creation_commit(&self) {
        self.0.lock().unwrap().fail_creation_commit_once = true;
    }
    pub fn replace_flow_for_test(&self, flow: TemporaryAuthFlow) {
        self.0.lock().unwrap().flows.insert(flow.id, flow);
    }
    pub fn authentication_counts(&self) -> (usize, usize, usize) {
        let state = self.0.lock().unwrap();
        (
            state.identities.len(),
            state.auth.len(),
            state.sessions.len(),
        )
    }

    pub fn lose_next_collision_ack(&self) {
        self.0.lock().unwrap().lose_collision_ack_once = true;
    }

    pub fn lose_flow_insert_ack_after(&self, inserts: usize) {
        self.0.lock().unwrap().lose_flow_insert_ack_after = Some(inserts);
    }

    pub fn fail_flow_insert_after(&self, inserts: usize) {
        self.0.lock().unwrap().fail_flow_insert_after = Some(inserts);
    }

    pub fn flows_snapshot(&self) -> Vec<TemporaryAuthFlow> {
        self.0.lock().unwrap().flows.values().cloned().collect()
    }
    // Downstream-only accessors: the hosted account service's end-to-end
    // tests (not in this repository) read these through the `test-support`
    // feature; this crate's own suite never does.
    #[cfg(feature = "test-support")]
    pub fn flow_snapshot(&self, id: AuthFlowId) -> Option<TemporaryAuthFlow> {
        self.0.lock().unwrap().flows.get(&id).cloned()
    }

    #[cfg(feature = "test-support")]
    pub fn session_snapshot(&self, id: AccountSessionId) -> Option<AccountSession> {
        self.0.lock().unwrap().sessions.get(&id).cloned()
    }

    pub fn discovery_counts(&self) -> (usize, usize) {
        let state = self.0.lock().unwrap();
        (state.profiles.len(), state.devices.len())
    }

    pub fn lose_next_link_ack(&self) {
        self.0.lock().unwrap().lose_link_ack_once = true;
    }
    pub fn lose_next_rebind_ack(&self) {
        self.0.lock().unwrap().lose_rebind_ack_once = true;
    }

    pub fn seed_erased_profile(&self, identity: KoruIdentity, profile: ProfileBinding) {
        let mut state = self.0.lock().unwrap();
        state.identities.insert(identity.id, identity);
        state.profiles.insert(profile.profile_id, profile);
    }

    pub fn seed_device_profile_state(
        &self,
        device_id: DeviceId,
        profile_id: ProfileId,
        state: DeviceProfileState,
    ) {
        self.0
            .lock()
            .unwrap()
            .device_profiles
            .insert((device_id, profile_id), state);
    }

    /// Seeds an enabled US Cloud Workspace membership for discovery and
    /// selection. `sync_scopes` false models a member without the three
    /// required scopes, which discovery must not list.
    pub fn seed_cloud_workspace_membership(
        &self,
        workspace_id: WorkspaceId,
        profile_id: ProfileId,
        sync_scopes: bool,
    ) {
        let mut state = self.0.lock().unwrap();
        state
            .cloud_workspaces
            .entry(workspace_id)
            .or_insert(FakeCloudWorkspace {
                incarnation_id: uuid::Uuid::from_u128(u128::from_le_bytes(
                    workspace_id.as_uuid().into_bytes(),
                )),
                authorization_version: 1,
                membership_epoch: 1,
                cloud_access_epoch: 1,
                enabled: true,
            });
        state
            .cloud_members
            .insert((profile_id, workspace_id), sync_scopes);
    }

    /// Models the worker having published the Workspace manifest so its
    /// original Store document id is retained in the hosted catalog.
    pub fn seed_cloud_manifest(
        &self,
        workspace_id: WorkspaceId,
        logical_replica_id: uuid::Uuid,
        manifest_document_id: uuid::Uuid,
    ) {
        self.0
            .lock()
            .unwrap()
            .cloud_manifests
            .insert(workspace_id, (logical_replica_id, manifest_document_id));
    }

    pub fn cloud_workspace_snapshot(
        &self,
        workspace_id: WorkspaceId,
    ) -> Option<FakeCloudWorkspace> {
        self.0
            .lock()
            .unwrap()
            .cloud_workspaces
            .get(&workspace_id)
            .copied()
    }

    pub fn device_profile_state(
        &self,
        device_id: DeviceId,
        profile_id: ProfileId,
    ) -> Option<DeviceProfileState> {
        self.0
            .lock()
            .unwrap()
            .device_profiles
            .get(&(device_id, profile_id))
            .copied()
    }

    /// Test-only model of PostgreSQL's identity-owned cascade. Local content
    /// is intentionally not an argument: the hosted repository cannot reach
    /// it, which the API erasure journey test holds as a separate sentinel.
    pub fn erase_hosted_identity(&self, identity: KoruIdentityId) -> ServerErasureCounts {
        let mut state = self.0.lock().unwrap();
        let auth_ids = state
            .auth
            .values()
            .filter(|auth| auth.koru_identity_id == identity)
            .map(|auth| auth.id)
            .collect::<BTreeSet<_>>();
        let profile_ids = state
            .profiles
            .values()
            .filter(|profile| profile.koru_identity_id == identity)
            .map(|profile| profile.profile_id)
            .collect::<BTreeSet<_>>();
        let device_ids = state
            .devices
            .values()
            .filter(|device| device.registration.koru_identity_id == identity)
            .map(AccountDevice::device_id)
            .collect::<BTreeSet<_>>();
        let counts = ServerErasureCounts {
            koru_identities: usize::from(state.identities.contains_key(&identity)) as u64,
            auth_identities: auth_ids.len() as u64,
            profile_bindings: profile_ids.len() as u64,
            profile_auth_bindings: state
                .profile_auth
                .iter()
                .filter(|(profile, auth)| profile_ids.contains(profile) || auth_ids.contains(auth))
                .count() as u64,
            devices: device_ids.len() as u64,
            device_profile_bindings: state
                .device_profiles
                .keys()
                .filter(|(_, profile)| profile_ids.contains(profile))
                .count() as u64,
            sessions: state
                .sessions
                .values()
                .filter(|session| session.koru_identity_id == identity)
                .count() as u64,
            flows: state
                .flows
                .values()
                .filter(|flow| flow.target_identity_id == Some(identity))
                .count() as u64,
            rate_buckets: 0,
        };
        state.identities.remove(&identity);
        state.auth.retain(|id, _| !auth_ids.contains(id));
        state.profiles.retain(|id, _| !profile_ids.contains(id));
        state
            .profile_auth
            .retain(|(profile, auth)| !profile_ids.contains(profile) && !auth_ids.contains(auth));
        state.devices.retain(|(owner, _), _| *owner != identity);
        state
            .device_profiles
            .retain(|(_, profile), _| !profile_ids.contains(profile));
        state
            .pending_enrollment
            .retain(|(_, profile), _| !profile_ids.contains(profile));
        state
            .sessions
            .retain(|_, session| session.koru_identity_id != identity);
        state
            .flows
            .retain(|_, flow| flow.target_identity_id != Some(identity));
        counts
    }
}

impl AccountExchangeRepository for Repository {
    fn commit_exchange<'a>(
        &'a self,
        request: ExchangeCommitRequest,
    ) -> PortFuture<'a, Result<ExchangeCommitOutcome, RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let flow = state
                .flows
                .get(&request.flow_id)
                .cloned()
                .ok_or(RepositoryError::NotFound)?;
            if flow.state_digest != request.grant_digest
                || request.now_millis >= flow.expires_at_millis
            {
                return Err(RepositoryError::Conflict);
            }
            if flow.status == AuthFlowStatus::Consumed {
                if state.proof_commits.get(&request.flow_id) != request.proof_commit_digest.as_ref()
                {
                    return Err(RepositoryError::Conflict);
                }
                return flow
                    .sealed_exchange_result
                    .map(ExchangeCommitOutcome::ExactRetry)
                    .ok_or(RepositoryError::Corrupt);
            }
            if flow.status != AuthFlowStatus::Pending
                || flow.target_identity_id != Some(request.session.koru_identity_id)
                || request.device.is_some() != request.session.device_id.is_some()
                || state.sessions.contains_key(&request.session.id)
            {
                return Err(RepositoryError::Conflict);
            }
            if let Some(device) = &request.device {
                if device.registration.koru_identity_id != request.session.koru_identity_id
                    || request.session.device_id != Some(device.device_id())
                {
                    return Err(RepositoryError::Conflict);
                }
                // Physical binding, rename ownership and revoked registration
                // checks belong to the shared registration transaction below.
            }
            if let Some(device) = request.device {
                state.register_account_device(
                    device,
                    request.device_ownership,
                    Some(request.flow_id),
                    request.now_millis,
                )?;
            }
            if let Some(digest) = request.proof_commit_digest {
                state.proof_commits.insert(request.flow_id, digest);
            }
            state.sessions.insert(request.session.id, request.session);
            let flow = state.flows.get_mut(&request.flow_id).unwrap();
            flow.status = AuthFlowStatus::Consumed;
            flow.consumed_at_millis = Some(request.now_millis);
            flow.sealed_exchange_result = Some(request.sealed_result);
            Ok(ExchangeCommitOutcome::Committed)
        })
    }
}

impl AccountProofRepository for Repository {
    fn commit_link_proof<'a>(
        &'a self,
        request: LinkProofCommitRequest,
    ) -> PortFuture<'a, Result<LinkProofCommitOutcome, LinkProofCommitError>> {
        Box::pin(async move {
            if request.mutation_digest.0.len() != 32 {
                return Err(LinkProofCommitError::ProofInvalid);
            }
            let mut state = self.0.lock().unwrap();
            let proof = state
                .flows
                .get(&request.proof_flow_id)
                .ok_or(LinkProofCommitError::ProofInvalid)?;
            let parent_valid = state
                .flows
                .get(&request.parent_flow_id)
                .is_some_and(|parent| {
                    parent.kind == AuthFlowKind::LinkAuthIdentity
                        && parent.target_identity_id == Some(request.auth_identity.koru_identity_id)
                });
            if proof.kind != AuthFlowKind::LinkAuthIdentity
                || proof.state_digest != request.proof_grant_digest
                || proof.target_identity_id != Some(request.auth_identity.koru_identity_id)
                || !parent_valid
                || !state
                    .profiles
                    .get(&request.profile_id)
                    .is_some_and(|profile| {
                        profile.koru_identity_id == request.auth_identity.koru_identity_id
                    })
            {
                return Err(LinkProofCommitError::ProofInvalid);
            }
            if proof.status != AuthFlowStatus::Consumed
                && (proof.status == AuthFlowStatus::Expired
                    || request.now_millis >= proof.expires_at_millis)
            {
                return Err(LinkProofCommitError::ProofExpired);
            }
            if proof.status == AuthFlowStatus::Pending {
                let elapsed = request
                    .now_millis
                    .checked_sub(request.authenticated_at_millis)
                    .ok_or(LinkProofCommitError::ProofInvalid)?;
                if elapsed < 0
                    || elapsed >= 300_000
                    || !state.auth.get(&request.authorized_by).is_some_and(|auth| {
                        auth.provider == AuthProvider::Google
                            && auth.koru_identity_id == request.auth_identity.koru_identity_id
                            && auth.last_authenticated_at_millis >= request.authenticated_at_millis
                    })
                {
                    return Err(LinkProofCommitError::ProofInvalid);
                }
            }
            if proof.status == AuthFlowStatus::Consumed {
                if state.proof_commits.get(&request.proof_flow_id) != Some(&request.mutation_digest)
                {
                    return Err(LinkProofCommitError::ProofAlreadyUsed);
                }
                let existing = state
                    .auth
                    .values()
                    .find(|value| {
                        value.issuer == request.auth_identity.issuer
                            && value.subject == request.auth_identity.subject
                            && value.koru_identity_id == request.auth_identity.koru_identity_id
                            && state.profile_auth.contains(&(request.profile_id, value.id))
                    })
                    .cloned()
                    .ok_or(LinkProofCommitError::Corrupt)?;
                return Ok(LinkProofCommitOutcome::ExactRetry(existing));
            }
            if proof.status != AuthFlowStatus::Pending {
                return Err(LinkProofCommitError::ProofAlreadyUsed);
            }
            let existing = state
                .auth
                .values()
                .find(|value| {
                    value.issuer == request.auth_identity.issuer
                        && value.subject == request.auth_identity.subject
                })
                .cloned();
            let (stored, outcome) = match existing {
                Some(value) if value.koru_identity_id == request.auth_identity.koru_identity_id => {
                    (value.clone(), AuthProfileLinkCommit::AlreadyLinked(value))
                }
                Some(_) => return Err(LinkProofCommitError::IdentityOwnedByAnother),
                None => {
                    state
                        .auth
                        .insert(request.auth_identity.id, request.auth_identity.clone());
                    (
                        request.auth_identity.clone(),
                        AuthProfileLinkCommit::Linked(request.auth_identity),
                    )
                }
            };
            state.profile_auth.insert((request.profile_id, stored.id));
            let proof = state.flows.get_mut(&request.proof_flow_id).unwrap();
            proof.status = AuthFlowStatus::Consumed;
            proof.consumed_at_millis = Some(request.now_millis);
            state
                .proof_commits
                .insert(request.proof_flow_id, request.mutation_digest);
            let persisted = match outcome {
                AuthProfileLinkCommit::Linked(value)
                | AuthProfileLinkCommit::AlreadyLinked(value) => value,
            };
            if state.lose_link_ack_once {
                state.lose_link_ack_once = false;
                return Err(LinkProofCommitError::Unavailable);
            }
            Ok(LinkProofCommitOutcome::Committed {
                auth_identity: persisted,
                authorization: LinkAuthorizationCommitStatus::Added,
            })
        })
    }

    fn cancel_link_proof<'a>(
        &'a self,
        request: LinkProofCancelRequest,
    ) -> PortFuture<'a, Result<LinkProofCancelOutcome, LinkProofCommitError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let proof = state
                .flows
                .get(&request.proof_flow_id)
                .ok_or(LinkProofCommitError::ProofInvalid)?;
            if proof.kind != AuthFlowKind::LinkAuthIdentity
                || proof.state_digest != request.proof_grant_digest
                || proof.target_identity_id != Some(request.koru_identity_id)
            {
                return Err(LinkProofCommitError::ProofInvalid);
            }
            if proof.status == AuthFlowStatus::Consumed {
                return Ok(
                    if state.proof_commits.contains_key(&request.proof_flow_id) {
                        LinkProofCancelOutcome::AlreadyConfirmed
                    } else {
                        LinkProofCancelOutcome::ExactRetry
                    },
                );
            }
            if proof.status != AuthFlowStatus::Pending
                || request.now_millis >= proof.expires_at_millis
            {
                return Err(LinkProofCommitError::ProofExpired);
            }
            let proof = state.flows.get_mut(&request.proof_flow_id).unwrap();
            proof.status = AuthFlowStatus::Consumed;
            proof.consumed_at_millis = Some(request.now_millis);
            Ok(LinkProofCancelOutcome::Cancelled)
        })
    }

    fn commit_rebind_proof<'a>(
        &'a self,
        request: RebindProofCommitRequest,
    ) -> PortFuture<'a, Result<RebindProofCommitOutcome, RebindProofCommitError>> {
        Box::pin(async move {
            if request.mutation_digest.0.len() != 32 {
                return Err(RebindProofCommitError::ProofInvalid);
            }
            if !request.rebind.explicit_rebind {
                return Err(RebindProofCommitError::IntentRequired);
            }
            if request.now_millis != request.rebind.now_millis {
                return Err(RebindProofCommitError::AuthenticationInvalid);
            }
            let mut state = self.0.lock().unwrap();
            let proof = state
                .flows
                .get(&request.proof_flow_id)
                .ok_or(RebindProofCommitError::ProofInvalid)?;
            let parent_valid = state
                .flows
                .get(&request.parent_flow_id)
                .is_some_and(|parent| {
                    parent.kind == AuthFlowKind::LinkAuthIdentity
                        && parent.target_identity_id
                            == Some(request.rebind.profile.koru_identity_id)
                });
            if proof.kind != AuthFlowKind::LinkAuthIdentity
                || proof.state_digest != request.proof_grant_digest
                || proof.target_identity_id != Some(request.rebind.profile.koru_identity_id)
                || !parent_valid
            {
                return Err(RebindProofCommitError::ProofInvalid);
            }
            if proof.status != AuthFlowStatus::Consumed
                && (proof.status == AuthFlowStatus::Expired
                    || request.now_millis >= proof.expires_at_millis)
            {
                return Err(RebindProofCommitError::ProofExpired);
            }
            if proof.status == AuthFlowStatus::Consumed {
                if state.proof_commits.get(&request.proof_flow_id) != Some(&request.mutation_digest)
                {
                    return Err(RebindProofCommitError::ProofAlreadyUsed);
                }
                let stored = state
                    .profiles
                    .get(&request.rebind.profile.profile_id)
                    .filter(|profile| {
                        profile.koru_identity_id == request.rebind.profile.koru_identity_id
                            && state.profile_auth.contains(&(
                                request.rebind.authorization.profile_id,
                                request.rebind.authorization.auth_identity_id,
                            ))
                    })
                    .cloned()
                    .ok_or(RebindProofCommitError::Corrupt)?;
                return Ok(RebindProofCommitOutcome::ExactRetry(stored));
            }
            if proof.status != AuthFlowStatus::Pending {
                return Err(RebindProofCommitError::ProofAlreadyUsed);
            }
            let elapsed = request
                .rebind
                .now_millis
                .checked_sub(request.rebind.authenticated_at_millis)
                .ok_or(RebindProofCommitError::AuthenticationInvalid)?;
            if elapsed < 0 || elapsed >= 300_000 {
                return Err(RebindProofCommitError::AuthenticationInvalid);
            }
            if state
                .identities
                .contains_key(&request.rebind.erased_identity_id)
            {
                return Err(RebindProofCommitError::OldIdentityStillLive);
            }
            if !state
                .auth
                .get(&request.authenticated_by)
                .is_some_and(|auth| {
                    auth.provider == AuthProvider::Google
                        && auth.koru_identity_id == request.rebind.profile.koru_identity_id
                        && auth.last_authenticated_at_millis
                            >= request.rebind.authenticated_at_millis
                })
            {
                return Err(RebindProofCommitError::AuthenticationInvalid);
            }
            let inserted =
                if let Some(stored) = state.profiles.get(&request.rebind.profile.profile_id) {
                    if stored.koru_identity_id != request.rebind.profile.koru_identity_id {
                        return Err(RebindProofCommitError::ProfileOwnedByAnother);
                    }
                    false
                } else {
                    state.profiles.insert(
                        request.rebind.profile.profile_id,
                        request.rebind.profile.clone(),
                    );
                    true
                };
            state.profile_auth.insert((
                request.rebind.authorization.profile_id,
                request.rebind.authorization.auth_identity_id,
            ));
            let proof = state.flows.get_mut(&request.proof_flow_id).unwrap();
            proof.status = AuthFlowStatus::Consumed;
            proof.consumed_at_millis = Some(request.rebind.now_millis);
            state
                .proof_commits
                .insert(request.proof_flow_id, request.mutation_digest);
            let _ = inserted;
            if state.lose_rebind_ack_once {
                state.lose_rebind_ack_once = false;
                return Err(RebindProofCommitError::Unavailable);
            }
            Ok(RebindProofCommitOutcome::Committed(request.rebind.profile))
        })
    }
}

impl IdentityRepository for Repository {
    fn find_koru_identity<'a>(
        &'a self,
        id: KoruIdentityId,
    ) -> PortFuture<'a, Result<Option<KoruIdentity>, RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            state.owner_reads += 1;
            if state.fail_owner_reads {
                return Err(RepositoryError::Unavailable);
            }
            Ok(state.identities.get(&id).cloned())
        })
    }
    fn find_auth_identity<'a>(
        &'a self,
        issuer: &'a str,
        subject: &'a str,
    ) -> PortFuture<'a, Result<Option<AuthIdentity>, RepositoryError>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .unwrap()
                .auth
                .values()
                .find(|a| a.issuer == issuer && a.subject == subject)
                .cloned())
        })
    }
    fn list_auth_identities<'a>(
        &'a self,
        id: KoruIdentityId,
    ) -> PortFuture<'a, Result<Vec<AuthIdentity>, RepositoryError>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .unwrap()
                .auth
                .values()
                .filter(|a| a.koru_identity_id == id)
                .cloned()
                .collect())
        })
    }
    fn update_identity_display_name<'a>(
        &'a self,
        id: KoruIdentityId,
        display_name: &'a str,
        updated_at_millis: UnixMillis,
    ) -> PortFuture<'a, Result<KoruIdentity, RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let identity = state
                .identities
                .get_mut(&id)
                .ok_or(RepositoryError::NotFound)?;
            identity.display_name = display_name.to_owned();
            identity.updated_at_millis = identity.updated_at_millis.max(updated_at_millis);
            Ok(identity.clone())
        })
    }
    fn create_identity_with_auth<'a>(
        &'a self,
        identity: KoruIdentity,
        auth: AuthIdentity,
    ) -> PortFuture<'a, Result<(), RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            if identity.id != auth.koru_identity_id
                || state
                    .auth
                    .values()
                    .any(|a| a.issuer == auth.issuer && a.subject == auth.subject)
            {
                return Err(RepositoryError::Conflict);
            }
            state.identities.insert(identity.id, identity);
            state.auth.insert(auth.id, auth);
            Ok(())
        })
    }
    fn link_auth_identity<'a>(
        &'a self,
        auth: AuthIdentity,
    ) -> PortFuture<'a, Result<(), RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            if !state.identities.contains_key(&auth.koru_identity_id)
                || state
                    .auth
                    .values()
                    .any(|a| a.issuer == auth.issuer && a.subject == auth.subject)
            {
                return Err(RepositoryError::Conflict);
            }
            state.auth.insert(auth.id, auth);
            Ok(())
        })
    }
}

impl IdentityCollisionRepository for Repository {
    fn find_koru_identities_by_verified_email<'a>(
        &'a self,
        email: &'a str,
    ) -> PortFuture<'a, Result<Vec<KoruIdentityId>, RepositoryError>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .unwrap()
                .auth
                .values()
                .filter_map(|a| {
                    a.verified_email
                        .as_ref()
                        .filter(|e| e.normalized_email == email)
                        .map(|_| a.koru_identity_id)
                })
                .collect())
        })
    }
}

/// One row of `email_challenges`, as this fake holds it.
#[derive(Clone)]
struct EmailChallengeRow {
    address_digest: KeyedDigest,
    code_digest: KeyedDigest,
    attempts: i32,
    last_sent_at_millis: UnixMillis,
}

/// Email sign-in's repository, in memory.
///
/// It exists because the e2e harness drives the real routes, and the only
/// thing between those routes and this file is SQL. The refusing stub it
/// replaced is why two defects in the native return URL reached an acceptance
/// checklist instead of a test.
///
/// The semantics mirror the hosted service's Postgres repository (not in this
/// repository) deliberately: one
/// attempt per `record_attempt` call, a resend that replaces the code and
/// resets attempts, an address digest unique across credentials, and a sign-up
/// that either writes every row or none of them.
impl koru_account_core::EmailRepository for Repository {
    fn credential_for_address<'a>(
        &'a self,
        address_digest: &'a KeyedDigest,
    ) -> PortFuture<'a, Result<Option<koru_account_core::EmailCredential>, RepositoryError>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .unwrap()
                .email_credentials
                .get(&address_digest.0)
                .cloned())
        })
    }

    fn insert_challenge<'a>(
        &'a self,
        flow_id: AuthFlowId,
        address_digest: &'a KeyedDigest,
        code_digest: &'a KeyedDigest,
        now_millis: UnixMillis,
    ) -> PortFuture<'a, Result<(), RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            if state.email_challenges.contains_key(&flow_id) {
                return Err(RepositoryError::Conflict);
            }
            state.email_challenges.insert(
                flow_id,
                EmailChallengeRow {
                    address_digest: address_digest.clone(),
                    code_digest: code_digest.clone(),
                    attempts: 0,
                    last_sent_at_millis: now_millis,
                },
            );
            Ok(())
        })
    }

    fn challenge<'a>(
        &'a self,
        flow_id: AuthFlowId,
    ) -> PortFuture<'a, Result<Option<koru_account_core::ChallengeState>, RepositoryError>> {
        Box::pin(async move {
            let state = self.0.lock().unwrap();
            let Some(row) = state.email_challenges.get(&flow_id) else {
                return Ok(None);
            };
            // Joined with the flow, exactly as the SQL is: expiry and
            // consumption live there, not on the challenge.
            let flow = state.flows.get(&flow_id).ok_or(RepositoryError::NotFound)?;
            Ok(Some(koru_account_core::ChallengeState {
                address_digest: row.address_digest.clone(),
                code_digest: row.code_digest.clone(),
                attempts: row.attempts,
                last_sent_at_millis: row.last_sent_at_millis,
                expires_at_millis: flow.expires_at_millis,
                consumed: flow.status == AuthFlowStatus::Consumed,
            }))
        })
    }

    fn record_attempt<'a>(
        &'a self,
        flow_id: AuthFlowId,
    ) -> PortFuture<'a, Result<i32, RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let row = state
                .email_challenges
                .get_mut(&flow_id)
                .ok_or(RepositoryError::NotFound)?;
            row.attempts += 1;
            Ok(row.attempts)
        })
    }

    fn record_send<'a>(
        &'a self,
        flow_id: AuthFlowId,
        code_digest: &'a KeyedDigest,
        now_millis: UnixMillis,
    ) -> PortFuture<'a, Result<(), RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let row = state
                .email_challenges
                .get_mut(&flow_id)
                .ok_or(RepositoryError::NotFound)?;
            row.code_digest = code_digest.clone();
            row.last_sent_at_millis = now_millis;
            row.attempts = 0;
            Ok(())
        })
    }

    fn complete_email_signup<'a>(
        &'a self,
        commit: koru_account_core::EmailSignupCommit,
    ) -> PortFuture<'a, Result<koru_account_core::EmailSignupOutcome, AccountCreationError>> {
        Box::pin(async move {
            use koru_account_core::EmailSignupOutcome;
            let mut locked = self.0.lock().unwrap();
            // Staged on a clone, so a refusal leaves nothing behind -- the
            // same property the real transaction has.
            let mut state = locked.clone();
            let flow = state
                .flows
                .get(&commit.flow_id)
                .cloned()
                .ok_or(AccountCreationError::FlowNotFound)?;
            if !matches!(
                flow.kind,
                AuthFlowKind::EmailWeb | AuthFlowKind::EmailNative
            ) || commit.state_digest.0.len() != 32
                || flow.state_digest != commit.state_digest
                || flow.target_identity_id.is_some()
                || commit.now_millis < flow.created_at_millis
                || commit.now_millis >= flow.expires_at_millis
                || flow.status == AuthFlowStatus::Expired
            {
                return Err(AccountCreationError::InvalidBinding);
            }
            if flow.status == AuthFlowStatus::Consumed {
                if state.proof_commits.get(&flow.id) != Some(&commit.state_digest) {
                    return Err(AccountCreationError::InvalidBinding);
                }
                return Ok(EmailSignupOutcome::Committed {
                    sealed_terminal_result: flow
                        .sealed_exchange_result
                        .ok_or(AccountCreationError::Corrupt)?,
                    replayed: true,
                });
            }
            if state
                .email_credentials
                .contains_key(&commit.address_digest.0)
            {
                return Ok(EmailSignupOutcome::AddressTaken);
            }
            state
                .identities
                .insert(commit.identity.id, commit.identity.clone());
            state
                .auth
                .insert(commit.auth_identity.id, commit.auth_identity.clone());
            state.email_credentials.insert(
                commit.address_digest.0.clone(),
                koru_account_core::EmailCredential {
                    auth_identity_id: commit.auth_identity.id,
                    koru_identity_id: commit.identity.id,
                    verifier: commit.verifier.clone(),
                },
            );
            // The grant, written with the account it lets in.
            state
                .flows
                .insert(commit.grant_flow.id, commit.grant_flow.clone());
            let consumed = state
                .flows
                .get_mut(&commit.flow_id)
                .ok_or(AccountCreationError::Corrupt)?;
            consumed.status = AuthFlowStatus::Consumed;
            consumed.sealed_exchange_result = Some(commit.sealed_terminal_result.clone());
            state
                .proof_commits
                .insert(commit.flow_id, commit.state_digest.clone());
            *locked = state;
            Ok(EmailSignupOutcome::Committed {
                sealed_terminal_result: commit.sealed_terminal_result,
                replayed: false,
            })
        })
    }

    fn replace_verifier<'a>(
        &'a self,
        auth_identity_id: AuthIdentityId,
        verifier: &'a koru_account_core::PasswordVerifier,
        _now_millis: UnixMillis,
    ) -> PortFuture<'a, Result<(), RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let credential = state
                .email_credentials
                .values_mut()
                .find(|credential| credential.auth_identity_id == auth_identity_id)
                .ok_or(RepositoryError::NotFound)?;
            credential.verifier = verifier.clone();
            Ok(())
        })
    }
}

impl AuthFlowRepository for Repository {
    fn exchange_proof_digest<'a>(
        &'a self,
        id: AuthFlowId,
    ) -> PortFuture<'a, Result<Option<KeyedDigest>, RepositoryError>> {
        Box::pin(async move { Ok(self.0.lock().unwrap().proof_commits.get(&id).cloned()) })
    }
    fn insert_flow<'a>(
        &'a self,
        flow: TemporaryAuthFlow,
    ) -> PortFuture<'a, Result<(), RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            if state.flows.contains_key(&flow.id) {
                return Err(RepositoryError::Conflict);
            }
            match state.fail_flow_insert_after {
                Some(0) => {
                    state.fail_flow_insert_after = None;
                    return Err(RepositoryError::Unavailable);
                }
                Some(left) => state.fail_flow_insert_after = Some(left - 1),
                None => {}
            }
            state.flows.insert(flow.id, flow);
            match state.lose_flow_insert_ack_after {
                Some(0) => {
                    state.lose_flow_insert_ack_after = None;
                    return Err(RepositoryError::Unavailable);
                }
                Some(left) => state.lose_flow_insert_ack_after = Some(left - 1),
                None => {}
            }
            Ok(())
        })
    }
    fn find_flow<'a>(
        &'a self,
        id: AuthFlowId,
    ) -> PortFuture<'a, Result<Option<TemporaryAuthFlow>, RepositoryError>> {
        Box::pin(async move {
            let snapshot = self.0.lock().unwrap().flows.get(&id).cloned();
            let hook = self.1.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook(id, snapshot.clone()).await;
            }
            Ok(snapshot)
        })
    }
    fn consume_flow<'a>(
        &'a self,
        id: AuthFlowId,
        now: i64,
    ) -> PortFuture<'a, Result<TemporaryAuthFlow, RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let flow = state.flows.get_mut(&id).ok_or(RepositoryError::NotFound)?;
            if flow.status != AuthFlowStatus::Pending || now >= flow.expires_at_millis {
                return Err(RepositoryError::Conflict);
            }
            flow.status = AuthFlowStatus::Consumed;
            flow.consumed_at_millis = Some(now);
            Ok(flow.clone())
        })
    }
}

impl IdentityResolutionRepository for Repository {
    fn commit_account_creation<'a>(
        &'a self,
        request: AccountCreationRequest,
    ) -> PortFuture<'a, Result<AccountCreationOutcome, AccountCreationError>> {
        Box::pin(async move {
            let mut locked = self.0.lock().unwrap();
            // One mutex is this fake's transaction authority. Stage on a clone
            // so every injected failure rolls back identity and flow effects.
            let mut state = locked.clone();
            let flow = state
                .flows
                .get(&request.confirmation_flow_id)
                .cloned()
                .ok_or(AccountCreationError::FlowNotFound)?;
            if !matches!(
                request.kind,
                AuthFlowKind::GoogleWeb
                    | AuthFlowKind::GoogleNative
                    | AuthFlowKind::AppleWeb
                    | AuthFlowKind::AppleNative
            ) || flow.kind != request.kind
                || flow.state_digest != request.state_digest
                || request.state_digest.0.len() != 32
                || flow.sealed_transient != request.expected_sealed_artifact
                || flow.expires_at_millis != request.expires_at_millis
                || flow.target_identity_id.is_some()
            {
                return Err(AccountCreationError::InvalidBinding);
            }
            if request.now_millis < flow.created_at_millis
                || request.now_millis >= flow.expires_at_millis
                || flow.status == AuthFlowStatus::Expired
            {
                return Err(AccountCreationError::FlowExpired);
            }
            if flow.status == AuthFlowStatus::Consumed {
                if state.proof_commits.get(&flow.id) != Some(&request.state_digest) {
                    return Err(AccountCreationError::InvalidBinding);
                }
                return Ok(AccountCreationOutcome::Terminal {
                    sealed_terminal_result: flow
                        .sealed_exchange_result
                        .ok_or(AccountCreationError::Corrupt)?,
                    replayed: true,
                });
            }
            if flow.sealed_exchange_result.is_some() {
                return Err(AccountCreationError::Corrupt);
            }
            let result = match request.decision {
                AccountCreationDecision::Cancel {
                    sealed_terminal_result,
                } => sealed_terminal_result,
                AccountCreationDecision::Confirm {
                    verified,
                    verified_at_millis,
                    expected,
                    new_identity,
                    new_auth_identity,
                    child_flows,
                    sealed_terminal_result,
                } => {
                    let exact = state.auth.values().find(|auth| {
                        auth.issuer == verified.canonical_issuer && auth.subject == verified.subject
                    });
                    let current = if let Some(auth) = exact {
                        let identity = state
                            .identities
                            .get(&auth.koru_identity_id)
                            .ok_or(AccountCreationError::Corrupt)?;
                        if identity.status != KoruIdentityStatus::Active {
                            return Err(AccountCreationError::IdentityDisabled);
                        }
                        CreationResolution::Returning {
                            koru_identity_id: identity.id,
                            auth_identity_id: auth.id,
                        }
                    } else {
                        let targets = state
                            .auth
                            .values()
                            .filter(|auth| {
                                verified.verified_email.is_some()
                                    && auth
                                        .verified_email
                                        .as_ref()
                                        .map(|email| &email.normalized_email)
                                        == verified.verified_email.as_ref()
                            })
                            .map(|auth| auth.koru_identity_id)
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect::<Vec<_>>();
                        if targets.is_empty() {
                            CreationResolution::Unseen
                        } else {
                            CreationResolution::Collision { targets }
                        }
                    };
                    if current != expected {
                        return Ok(AccountCreationOutcome::Reprepare(current));
                    }
                    if verified_at_millis > request.now_millis
                        || child_flows.is_empty()
                        || child_flows.len() > 2
                    {
                        return Err(AccountCreationError::InvalidPreparation);
                    }
                    let target = match expected {
                        CreationResolution::Unseen => {
                            let identity =
                                new_identity.ok_or(AccountCreationError::InvalidPreparation)?;
                            let auth = new_auth_identity
                                .ok_or(AccountCreationError::InvalidPreparation)?;
                            if state.identities.contains_key(&identity.id)
                                || state.auth.contains_key(&auth.id)
                                || auth.koru_identity_id != identity.id
                                || auth.issuer != verified.canonical_issuer
                                || auth.subject != verified.subject
                                || identity.status != KoruIdentityStatus::Active
                                || identity.created_at_millis != request.now_millis
                                || identity.terms.accepted_at_millis != request.now_millis
                                || auth.created_at_millis != request.now_millis
                                || auth.last_authenticated_at_millis != request.now_millis
                            {
                                return Err(AccountCreationError::InvalidPreparation);
                            }
                            let target = identity.id;
                            state.identities.insert(identity.id, identity);
                            state.auth.insert(auth.id, auth);
                            Some(target)
                        }
                        CreationResolution::Returning {
                            koru_identity_id,
                            auth_identity_id,
                        } => {
                            if new_identity.is_some() || new_auth_identity.is_some() {
                                return Err(AccountCreationError::InvalidPreparation);
                            }
                            let auth = state.auth.get_mut(&auth_identity_id).unwrap();
                            auth.last_authenticated_at_millis =
                                auth.last_authenticated_at_millis.max(verified_at_millis);
                            Some(koru_identity_id)
                        }
                        CreationResolution::Collision { targets } => {
                            if new_identity.is_some()
                                || new_auth_identity.is_some()
                                || child_flows.len() != 2
                                || child_flows
                                    .iter()
                                    .filter(|child| {
                                        child.kind == AuthFlowKind::VerifiedEmailCollision
                                    })
                                    .count()
                                    != 1
                                || child_flows
                                    .iter()
                                    .filter(|child| {
                                        matches!(
                                            child.kind,
                                            AuthFlowKind::GoogleWeb
                                                | AuthFlowKind::GoogleNative
                                                | AuthFlowKind::AppleWeb
                                                | AuthFlowKind::AppleNative
                                        )
                                    })
                                    .count()
                                    != 1
                                || child_flows.iter().any(|child| {
                                    child
                                        .target_identity_id
                                        .is_some_and(|id| !targets.contains(&id))
                                })
                            {
                                return Err(AccountCreationError::InvalidPreparation);
                            }
                            None
                        }
                    };
                    if target.is_some()
                        && (child_flows.len() != 1
                            || child_flows[0].kind != request.kind
                            || child_flows[0].target_identity_id != target)
                    {
                        return Err(AccountCreationError::InvalidPreparation);
                    }
                    for child in child_flows {
                        if state.flows.contains_key(&child.id)
                            || child.status != AuthFlowStatus::Pending
                            || child.consumed_at_millis.is_some()
                            || child.sealed_exchange_result.is_some()
                            || child.state_digest.0.len() != 32
                            || child.sealed_transient.0.is_empty()
                            || child.source_digest.0.is_empty()
                            || child.created_at_millis > request.now_millis
                            || child.expires_at_millis <= request.now_millis
                            || child.expires_at_millis > request.expires_at_millis
                            || child
                                .target_identity_id
                                .is_some_and(|id| !state.identities.contains_key(&id))
                        {
                            return Err(AccountCreationError::InvalidPreparation);
                        }
                        state.flows.insert(child.id, child);
                    }
                    sealed_terminal_result
                }
            };
            if result.0.is_empty() || result.0.len() > 270_336 {
                return Err(AccountCreationError::InvalidPreparation);
            }
            let flow = state.flows.get_mut(&flow.id).unwrap();
            flow.status = AuthFlowStatus::Consumed;
            flow.consumed_at_millis = Some(request.now_millis);
            flow.sealed_exchange_result = Some(result.clone());
            state.proof_commits.insert(flow.id, request.state_digest);
            if locked.fail_creation_commit_once {
                locked.fail_creation_commit_once = false;
                return Err(AccountCreationError::Unavailable);
            }
            let lost = state.lose_creation_ack_once;
            state.lose_creation_ack_once = false;
            *locked = state;
            if lost {
                return Err(AccountCreationError::Unavailable);
            }
            Ok(AccountCreationOutcome::Terminal {
                sealed_terminal_result: result,
                replayed: false,
            })
        })
    }

    fn link_auth_identity_and_profile<'a>(
        &'a self,
        auth: AuthIdentity,
        profile_id: ProfileId,
    ) -> PortFuture<'a, Result<AuthProfileLinkCommit, AuthProfileLinkError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            if !state
                .profiles
                .get(&profile_id)
                .is_some_and(|profile| profile.koru_identity_id == auth.koru_identity_id)
            {
                return Err(AuthProfileLinkError::ProfileNotOwned);
            }
            let existing = state
                .auth
                .values()
                .find(|value| value.issuer == auth.issuer && value.subject == auth.subject)
                .cloned();
            let (stored, outcome) = match existing {
                Some(value) if value.koru_identity_id == auth.koru_identity_id => {
                    (value.clone(), AuthProfileLinkCommit::AlreadyLinked(value))
                }
                Some(_) => return Err(AuthProfileLinkError::IdentityOwnedByAnother),
                None => {
                    state.auth.insert(auth.id, auth.clone());
                    (auth.clone(), AuthProfileLinkCommit::Linked(auth))
                }
            };
            state.profile_auth.insert((profile_id, stored.id));
            Ok(outcome)
        })
    }

    fn record_authentication<'a>(
        &'a self,
        id: AuthIdentityId,
        at: i64,
    ) -> PortFuture<'a, Result<AuthIdentity, RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let auth = state.auth.get_mut(&id).ok_or(RepositoryError::NotFound)?;
            auth.last_authenticated_at_millis = auth.last_authenticated_at_millis.max(at);
            Ok(auth.clone())
        })
    }
    fn complete_collision_link<'a>(
        &'a self,
        flow_id: AuthFlowId,
        auth: AuthIdentity,
        now: i64,
    ) -> PortFuture<'a, Result<CollisionLinkCommit, CollisionLinkError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let flow = state
                .flows
                .get(&flow_id)
                .ok_or(CollisionLinkError::FlowNotFound)?;
            if flow.status != AuthFlowStatus::Pending {
                return Err(CollisionLinkError::FlowConsumed);
            }
            if now >= flow.expires_at_millis {
                return Err(CollisionLinkError::FlowExpired);
            }
            if let Some(existing) = state
                .auth
                .values()
                .find(|a| a.issuer == auth.issuer && a.subject == auth.subject)
            {
                return if existing.koru_identity_id == auth.koru_identity_id {
                    Ok(CollisionLinkCommit::AlreadyLinked(existing.clone()))
                } else {
                    Err(CollisionLinkError::IdentityOwnedByAnother)
                };
            }
            state.auth.insert(auth.id, auth.clone());
            let flow = state.flows.get_mut(&flow_id).unwrap();
            flow.status = AuthFlowStatus::Consumed;
            flow.consumed_at_millis = Some(now);
            if state.lose_collision_ack_once {
                state.lose_collision_ack_once = false;
                return Err(CollisionLinkError::Unavailable);
            }
            Ok(CollisionLinkCommit::Linked(auth))
        })
    }
}

impl ProfileRepository for Repository {
    fn find_profile<'a>(
        &'a self,
        id: ProfileId,
    ) -> PortFuture<'a, Result<Option<ProfileBinding>, RepositoryError>> {
        Box::pin(async move { Ok(self.0.lock().unwrap().profiles.get(&id).cloned()) })
    }
    fn list_profiles<'a>(
        &'a self,
        identity: KoruIdentityId,
        auth: AuthIdentityId,
    ) -> PortFuture<'a, Result<Vec<ProfileBinding>, RepositoryError>> {
        Box::pin(async move {
            let state = self.0.lock().unwrap();
            Ok(state
                .profiles
                .values()
                .filter(|p| {
                    p.koru_identity_id == identity
                        && p.lifecycle_state == ProfileLifecycleState::Active
                        && state.profile_auth.contains(&(p.profile_id, auth))
                })
                .cloned()
                .collect())
        })
    }
    fn attach_profile<'a>(
        &'a self,
        profile: ProfileBinding,
        auth: ProfileAuthBinding,
        device_id: Option<DeviceId>,
    ) -> PortFuture<'a, Result<ProfileAttachOutcome, RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            if profile.profile_id != auth.profile_id
                || !state
                    .auth
                    .get(&auth.auth_identity_id)
                    .is_some_and(|a| a.koru_identity_id == profile.koru_identity_id)
            {
                return Err(RepositoryError::Conflict);
            }
            if let Some(stored) = state.profiles.get(&profile.profile_id) {
                if stored.koru_identity_id != profile.koru_identity_id
                    || stored.lifecycle_state != ProfileLifecycleState::Active
                {
                    return Err(RepositoryError::Conflict);
                }
                let stored = stored.clone();
                if let Some(device_id) = device_id {
                    state.device_profiles.insert(
                        (device_id, profile.profile_id),
                        DeviceProfileState::Provisioned,
                    );
                }
                return Ok(
                    if state
                        .profile_auth
                        .insert((profile.profile_id, auth.auth_identity_id))
                    {
                        ProfileAttachOutcome::AuthorizationAdded(stored)
                    } else {
                        ProfileAttachOutcome::AlreadyAttached(stored)
                    },
                );
            }
            state.profiles.insert(profile.profile_id, profile.clone());
            state
                .profile_auth
                .insert((profile.profile_id, auth.auth_identity_id));
            if let Some(device_id) = device_id {
                state.device_profiles.insert(
                    (device_id, profile.profile_id),
                    DeviceProfileState::Provisioned,
                );
            }
            Ok(ProfileAttachOutcome::Attached(profile))
        })
    }
    fn delete_profile<'a>(
        &'a self,
        request: DeleteProfileRequest,
    ) -> PortFuture<'a, Result<ProfileLifecycleMutationOutcome, ProfileLifecycleMutationError>>
    {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let stored = state
                .profiles
                .get(&request.profile_id)
                .cloned()
                .ok_or(ProfileLifecycleMutationError::WrongIdentity)?;
            if stored.koru_identity_id != request.koru_identity_id {
                return Err(ProfileLifecycleMutationError::WrongIdentity);
            }
            if stored.lifecycle_state == ProfileLifecycleState::Deleted {
                if stored.lifecycle_operation_id == Some(request.operation_id) {
                    return Ok(ProfileLifecycleMutationOutcome {
                        status: ProfileLifecycleMutationStatus::ExactRetry,
                        lifecycle: lifecycle_from_binding(&stored),
                    });
                }
                return Err(ProfileLifecycleMutationError::AlreadyDeleted);
            }
            if stored.lifecycle_revision != request.expected_revision {
                return Err(ProfileLifecycleMutationError::StaleRevision);
            }
            if !state
                .profile_auth
                .contains(&(request.profile_id, request.authenticated_by))
            {
                return Err(ProfileLifecycleMutationError::PermissionDenied);
            }
            let device = state
                .devices
                .get(&(request.koru_identity_id, request.device_id))
                .filter(|device| {
                    device.registration.koru_identity_id == request.koru_identity_id
                        && device.registration.revoked_at_millis.is_none()
                })
                .ok_or(ProfileLifecycleMutationError::PermissionDenied)?;
            let _ = device;
            state
                .device_profiles
                .entry((request.device_id, request.profile_id))
                .or_insert(DeviceProfileState::Provisioned);
            if state
                .device_profiles
                .get(&(request.device_id, request.profile_id))
                == Some(&DeviceProfileState::Revoked)
            {
                return Err(ProfileLifecycleMutationError::PermissionDenied);
            }
            state
                .device_profiles
                .iter_mut()
                .filter(|((_, profile_id), _)| *profile_id == request.profile_id)
                .for_each(|(_, value)| *value = DeviceProfileState::Revoked);
            state
                .profile_auth
                .retain(|(profile_id, _)| *profile_id != request.profile_id);
            let updated = state.profiles.get_mut(&request.profile_id).unwrap();
            updated.lifecycle_state = ProfileLifecycleState::Deleted;
            updated.lifecycle_revision = updated
                .lifecycle_revision
                .next()
                .ok_or(ProfileLifecycleMutationError::Corrupt)?;
            updated.lifecycle_operation_id = Some(request.operation_id);
            updated.deleted_at_millis = Some(request.now_millis);
            updated.discovery_name_snapshot = None;
            updated.locale_snapshot = None;
            updated.updated_at_millis = request.now_millis;
            Ok(ProfileLifecycleMutationOutcome {
                status: ProfileLifecycleMutationStatus::Applied,
                lifecycle: lifecycle_from_binding(updated),
            })
        })
    }
    fn resolve_profile_lifecycles<'a>(
        &'a self,
        identity_id: KoruIdentityId,
        profile_ids: &'a [ProfileId],
    ) -> PortFuture<'a, Result<Vec<ProfileLifecycle>, RepositoryError>> {
        Box::pin(async move {
            let state = self.0.lock().unwrap();
            Ok(profile_ids
                .iter()
                .filter_map(|profile_id| state.profiles.get(profile_id))
                .filter(|profile| profile.koru_identity_id == identity_id)
                .map(lifecycle_from_binding)
                .collect())
        })
    }
    fn rebind_erased_profile<'a>(
        &'a self,
        request: ProfileRebindRequest,
    ) -> PortFuture<'a, Result<ProfileRebindOutcome, ProfileRebindError>> {
        Box::pin(async move {
            if !request.explicit_rebind {
                return Err(ProfileRebindError::IntentRequired);
            }
            let elapsed = request
                .now_millis
                .checked_sub(request.authenticated_at_millis)
                .ok_or(ProfileRebindError::AuthenticationInvalid)?;
            if elapsed < 0 || elapsed >= 300_000 {
                return Err(ProfileRebindError::AuthenticationInvalid);
            }
            let mut state = self.0.lock().unwrap();
            if state.identities.contains_key(&request.erased_identity_id) {
                return Err(ProfileRebindError::OldIdentityStillLive);
            }
            let valid = state
                .auth
                .get(&request.authorization.auth_identity_id)
                .is_some_and(|a| {
                    a.provider == AuthProvider::Google
                        && a.koru_identity_id == request.profile.koru_identity_id
                        && a.last_authenticated_at_millis >= request.authenticated_at_millis
                });
            if !valid {
                return Err(ProfileRebindError::AuthenticationInvalid);
            }
            if let Some(stored) = state.profiles.get(&request.profile.profile_id) {
                if stored.koru_identity_id != request.profile.koru_identity_id {
                    return Err(ProfileRebindError::ProfileOwnedByAnother);
                }
                return Ok(ProfileRebindOutcome::AlreadyRebound(stored.clone()));
            }
            state
                .profiles
                .insert(request.profile.profile_id, request.profile.clone());
            state.profile_auth.insert((
                request.profile.profile_id,
                request.authorization.auth_identity_id,
            ));
            Ok(ProfileRebindOutcome::Rebound(request.profile))
        })
    }
}

impl DeviceRepository for Repository {
    fn find_device<'a>(
        &'a self,
        identity_id: KoruIdentityId,
        id: DeviceId,
    ) -> PortFuture<'a, Result<Option<AccountDevice>, RepositoryError>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .unwrap()
                .devices
                .get(&(identity_id, id))
                .cloned())
        })
    }
    fn register_device<'a>(
        &'a self,
        device: AccountDevice,
        proof: Option<VerifiedDeviceOwnership>,
    ) -> PortFuture<'a, Result<DeviceRegistrationOutcome, RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let now = device.registration.last_seen_at_millis;
            state.register_account_device(device, proof, None, now)
        })
    }
    fn list_devices<'a>(
        &'a self,
        identity: KoruIdentityId,
    ) -> PortFuture<'a, Result<Vec<AccountDevice>, RepositoryError>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .unwrap()
                .devices
                .values()
                .filter(|d| d.registration.koru_identity_id == identity)
                .cloned()
                .collect())
        })
    }
    fn list_devices_for_session<'a>(
        &'a self,
        identity: KoruIdentityId,
        authenticated_by: AuthIdentityId,
        session_id: AccountSessionId,
        now: UnixMillis,
    ) -> PortFuture<'a, Result<AccountDevicePage, RepositoryError>> {
        Box::pin(async move {
            let state = self.0.lock().unwrap();
            let session = state
                .sessions
                .get(&session_id)
                .ok_or(RepositoryError::NotFound)?;
            if session.koru_identity_id != identity
                || session.authenticated_by != authenticated_by
                || session.revoked_at_millis.is_some()
                || now >= session.idle_expires_at_millis
                || now >= session.absolute_expires_at_millis
            {
                return Err(RepositoryError::Conflict);
            }
            let visible_profiles = state
                .profile_auth
                .iter()
                .filter(|(_, auth)| *auth == authenticated_by)
                .map(|(profile, _)| *profile)
                .collect::<BTreeSet<_>>();
            Ok(AccountDevicePage {
                devices: state
                    .devices
                    .values()
                    .filter(|device| device.registration.koru_identity_id == identity)
                    .cloned()
                    .map(|device| {
                        let profile_states = state
                            .device_profiles
                            .iter()
                            .filter(|((id, profile), _)| {
                                *id == device.device_id() && visible_profiles.contains(profile)
                            })
                            .map(|((_, profile_id), state)| DeviceProfileStateSummary {
                                profile_id: *profile_id,
                                state: *state,
                            })
                            .collect();
                        AccountDeviceWithProfiles {
                            device,
                            profile_states,
                        }
                    })
                    .collect(),
                truncated: false,
            })
        })
    }
    fn set_profile_state<'a>(
        &'a self,
        binding: DeviceProfileBinding,
    ) -> PortFuture<'a, Result<(), RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            if matches!(
                binding.state,
                DeviceProfileState::PendingProvisioning | DeviceProfileState::Provisioned
            ) || state
                .device_profiles
                .get(&(binding.device_id, binding.profile_id))
                == Some(&DeviceProfileState::Revoked)
                && binding.state != DeviceProfileState::Revoked
            {
                return Err(RepositoryError::Conflict);
            }
            state
                .device_profiles
                .insert((binding.device_id, binding.profile_id), binding.state);
            if binding.state == DeviceProfileState::Revoked {
                state
                    .pending_enrollment
                    .remove(&(binding.device_id, binding.profile_id));
            }
            Ok(())
        })
    }
    fn revoke_device<'a>(
        &'a self,
        identity: KoruIdentityId,
        id: DeviceId,
        at: i64,
    ) -> PortFuture<'a, Result<AccountDevice, RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let device = state
                .devices
                .get_mut(&(identity, id))
                .filter(|d| d.registration.koru_identity_id == identity)
                .ok_or(RepositoryError::Conflict)?;
            device.registration.revoked_at_millis.get_or_insert(at);
            let device = device.clone();
            for session in state.sessions.values_mut().filter(|session| {
                session.kind == SessionKind::Native
                    && session.koru_identity_id == identity
                    && session.device_id == Some(id)
            }) {
                session.revoked_at_millis.get_or_insert(at);
            }
            Ok(device)
        })
    }
}

impl EnrollmentRepository for Repository {
    fn list_enrollment_candidates<'a>(
        &'a self,
        query: EnrollmentCandidateQuery,
    ) -> PortFuture<'a, Result<EnrollmentCandidatePage, EnrollmentRepositoryError>> {
        Box::pin(async move {
            let state = self.0.lock().unwrap();
            let session = state.sessions.get(&query.session_id);
            let caller = state
                .devices
                .get(&(query.koru_identity_id, query.caller_device_id));
            if !session.is_some_and(|session| {
                session.koru_identity_id == query.koru_identity_id
                    && session.authenticated_by == query.authenticated_by
                    && session.device_id == Some(query.caller_device_id)
                    && session.kind == SessionKind::Native
                    && session.revoked_at_millis.is_none()
                    && query.now_millis < session.idle_expires_at_millis
                    && query.now_millis < session.absolute_expires_at_millis
            }) || !caller.is_some_and(|device| {
                device.registration.koru_identity_id == query.koru_identity_id
                    && device.registration.revoked_at_millis.is_none()
                    && device.registration.enrollment_protocol_version
                        == Some(query.enrollment_protocol_version)
                    && device.registration.peer_protocol_version
                        == Some(query.peer_protocol_version)
            }) || !state
                .profile_auth
                .contains(&(query.profile_id, query.authenticated_by))
                || state
                    .device_profiles
                    .get(&(query.caller_device_id, query.profile_id))
                    == Some(&DeviceProfileState::Revoked)
            {
                return Err(EnrollmentRepositoryError::PermissionDenied);
            }
            let mut candidates = state
                .devices
                .values()
                .filter(|device| {
                    device.registration.koru_identity_id == query.koru_identity_id
                        && device.device_id() != query.caller_device_id
                        && device.registration.revoked_at_millis.is_none()
                        && device.registration.enrollment_protocol_version
                            == Some(query.enrollment_protocol_version)
                        && device.registration.peer_protocol_version
                            == Some(query.peer_protocol_version)
                        && state
                            .device_profiles
                            .get(&(device.device_id(), query.profile_id))
                            == Some(&DeviceProfileState::Provisioned)
                })
                .cloned()
                .collect::<Vec<_>>();
            candidates.sort_by_key(AccountDevice::device_id);
            let truncated = candidates.len() > 64;
            Ok(EnrollmentCandidatePage {
                candidates: candidates
                    .into_iter()
                    .take(64)
                    .map(|device| EnrollmentCandidate {
                        descriptor: device.descriptor,
                        platform: device.registration.platform,
                        app_version: device.registration.app_version,
                        enrollment_protocol_version: device
                            .registration
                            .enrollment_protocol_version
                            .expect("filtered"),
                        peer_protocol_version: device
                            .registration
                            .peer_protocol_version
                            .expect("filtered"),
                    })
                    .collect(),
                truncated,
            })
        })
    }

    fn prepare_device_attestation<'a>(
        &'a self,
        request: PrepareDeviceAttestation,
    ) -> PortFuture<'a, Result<DeviceAttestationSubject, EnrollmentRepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let permitted = state
                .sessions
                .get(&request.session_id)
                .is_some_and(|session| {
                    session.koru_identity_id == request.koru_identity_id
                        && session.authenticated_by == request.authenticated_by
                        && session.device_id == Some(request.device_id)
                        && session.kind == SessionKind::Native
                        && session.revoked_at_millis.is_none()
                        && request.now_millis < session.idle_expires_at_millis
                        && request.now_millis < session.absolute_expires_at_millis
                })
                && state
                    .profile_auth
                    .contains(&(request.profile_id, request.authenticated_by));
            let device = state
                .devices
                .get(&(request.koru_identity_id, request.device_id))
                .cloned();
            if !permitted
                || !device.as_ref().is_some_and(|device| {
                    device.registration.koru_identity_id == request.koru_identity_id
                        && device.registration.revoked_at_millis.is_none()
                        && device.registration.enrollment_protocol_version.is_some()
                        && device.registration.peer_protocol_version.is_some()
                })
                || state
                    .device_profiles
                    .get(&(request.device_id, request.profile_id))
                    == Some(&DeviceProfileState::Revoked)
            {
                return Err(EnrollmentRepositoryError::PermissionDenied);
            }
            state
                .device_profiles
                .entry((request.device_id, request.profile_id))
                .or_insert(DeviceProfileState::Authorized);
            Ok(DeviceAttestationSubject {
                descriptor: device.expect("validated").descriptor,
                authorized_profile_ids: vec![request.profile_id],
            })
        })
    }

    fn bind_pending_device_attestation<'a>(
        &'a self,
        request: BindPendingDeviceAttestation,
    ) -> PortFuture<'a, Result<(), EnrollmentRepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let permitted = state
                .sessions
                .get(&request.session_id)
                .is_some_and(|session| {
                    session.koru_identity_id == request.koru_identity_id
                        && session.authenticated_by == request.authenticated_by
                        && session.device_id == Some(request.device_id)
                        && session.revoked_at_millis.is_none()
                })
                && state
                    .profile_auth
                    .contains(&(request.profile_id, request.authenticated_by));
            let key = (request.device_id, request.profile_id);
            if !permitted
                || !matches!(
                    state.device_profiles.get(&key),
                    Some(
                        DeviceProfileState::Authorized
                            | DeviceProfileState::PendingProvisioning
                            | DeviceProfileState::Provisioned
                    )
                )
            {
                return Err(EnrollmentRepositoryError::PermissionDenied);
            }
            if state.pending_enrollment.iter().any(|(other_key, pending)| {
                other_key != &key && pending.token_id == request.token_id
            }) {
                return Err(EnrollmentRepositoryError::Conflict);
            }
            if state.pending_enrollment.get(&key).is_some_and(|pending| {
                pending.expires_at_millis > request.now_millis
                    && (pending.token_id != request.token_id
                        || pending.attestation_digest != request.attestation_digest)
            }) {
                return Err(EnrollmentRepositoryError::Conflict);
            }
            if state.device_profiles.get(&key) != Some(&DeviceProfileState::Provisioned) {
                state
                    .device_profiles
                    .insert(key, DeviceProfileState::PendingProvisioning);
            }
            let completed_mutation = state
                .pending_enrollment
                .get(&key)
                .filter(|pending| {
                    pending.token_id == request.token_id
                        && pending.attestation_digest == request.attestation_digest
                })
                .and_then(|pending| pending.mutation_digest);
            state.pending_enrollment.insert(
                key,
                PendingEnrollment {
                    session_id: request.session_id,
                    token_id: request.token_id,
                    attestation_digest: request.attestation_digest,
                    expires_at_millis: request.expires_at_millis,
                    mutation_digest: completed_mutation,
                },
            );
            Ok(())
        })
    }

    fn complete_device_provisioning<'a>(
        &'a self,
        request: CompleteDeviceProvisioning,
    ) -> PortFuture<'a, Result<ProvisioningOutcome, EnrollmentRepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let key = (request.device_id, request.profile_id);
            let pending = state
                .pending_enrollment
                .get(&key)
                .cloned()
                .ok_or(EnrollmentRepositoryError::PermissionDenied)?;
            let exact = pending.session_id == request.session_id
                && pending.token_id == request.token_id
                && pending.attestation_digest == request.attestation_digest;
            if state.device_profiles.get(&key) == Some(&DeviceProfileState::Provisioned) {
                if exact && pending.mutation_digest == Some(request.mutation_digest) {
                    return Ok(ProvisioningOutcome::ExactRetry);
                }
                if pending.mutation_digest.is_some() {
                    return Err(EnrollmentRepositoryError::Replayed);
                }
            }
            let session_live = state
                .sessions
                .get(&request.session_id)
                .is_some_and(|session| {
                    session.koru_identity_id == request.koru_identity_id
                        && session.authenticated_by == request.authenticated_by
                        && session.device_id == Some(request.device_id)
                        && session.revoked_at_millis.is_none()
                        && request.now_millis < session.idle_expires_at_millis
                        && request.now_millis < session.absolute_expires_at_millis
                });
            if !session_live
                || !state
                    .profile_auth
                    .contains(&(request.profile_id, request.authenticated_by))
            {
                return Err(EnrollmentRepositoryError::PermissionDenied);
            }
            if !request.allow_new_completion || pending.expires_at_millis <= request.now_millis {
                return Err(EnrollmentRepositoryError::Expired);
            }
            if !exact {
                return Err(EnrollmentRepositoryError::Replayed);
            }
            let joining = state
                .devices
                .get(&(request.koru_identity_id, request.device_id));
            let peer = state
                .devices
                .get(&(request.koru_identity_id, request.existing_device_id));
            if !peer.is_some_and(|peer| {
                peer.registration.koru_identity_id == request.koru_identity_id
                    && peer.device_id() != request.device_id
                    && peer.registration.revoked_at_millis.is_none()
                    && peer.descriptor.signing_public_key.as_bytes()
                        == &request.existing_signing_public_key
                    && peer.descriptor.endpoint_id.as_bytes() == &request.existing_endpoint_id
                    && joining.is_some_and(|joining| {
                        joining.registration.enrollment_protocol_version
                            == peer.registration.enrollment_protocol_version
                            && joining.registration.peer_protocol_version
                                == peer.registration.peer_protocol_version
                    })
                    && state
                        .device_profiles
                        .get(&(peer.device_id(), request.profile_id))
                        == Some(&DeviceProfileState::Provisioned)
            }) {
                return Err(EnrollmentRepositoryError::PermissionDenied);
            }
            state
                .device_profiles
                .insert(key, DeviceProfileState::Provisioned);
            state
                .pending_enrollment
                .get_mut(&key)
                .expect("present")
                .mutation_digest = Some(request.mutation_digest);
            Ok(ProvisioningOutcome::Provisioned)
        })
    }
}

impl State {
    /// The artifact's principal predicate: live native session for this
    /// device, active account, unrevoked registration.
    fn cloud_principal_live(
        &self,
        identity: KoruIdentityId,
        authenticated_by: AuthIdentityId,
        session_id: AccountSessionId,
        device_id: DeviceId,
        now_millis: i64,
    ) -> bool {
        self.sessions.get(&session_id).is_some_and(|session| {
            session.koru_identity_id == identity
                && session.authenticated_by == authenticated_by
                && session.device_id == Some(device_id)
                && session.kind == SessionKind::Native
                && session.revoked_at_millis.is_none()
                && now_millis < session.idle_expires_at_millis
                && now_millis < session.absolute_expires_at_millis
        }) && self
            .identities
            .get(&identity)
            .is_some_and(|value| value.status == KoruIdentityStatus::Active)
            && self
                .devices
                .get(&(identity, device_id))
                .is_some_and(|device| device.registration.revoked_at_millis.is_none())
    }

    fn cloud_profile_edge(
        &self,
        identity: KoruIdentityId,
        authenticated_by: AuthIdentityId,
        profile_id: ProfileId,
    ) -> bool {
        self.profile_auth.contains(&(profile_id, authenticated_by))
            && self.profiles.get(&profile_id).is_some_and(|profile| {
                profile.koru_identity_id == identity
                    && profile.lifecycle_state == ProfileLifecycleState::Active
            })
    }

    fn cloud_eligible(&self, profile_id: ProfileId, workspace_id: WorkspaceId) -> bool {
        self.cloud_members.get(&(profile_id, workspace_id)) == Some(&true)
            && self
                .cloud_workspaces
                .get(&workspace_id)
                .is_some_and(|workspace| workspace.enabled)
    }
}

impl koru_account_core::cloud_selection::CloudSelectionRepository for Repository {
    fn discover_cloud_workspaces<'a>(
        &'a self,
        query: koru_account_core::cloud_selection::CloudWorkspaceDiscovery,
    ) -> PortFuture<
        'a,
        Result<
            koru_account_core::cloud_selection::CloudWorkspacePage,
            koru_account_core::cloud_selection::CloudSelectionError,
        >,
    > {
        use koru_account_core::cloud_selection::*;
        Box::pin(async move {
            let state = self.0.lock().unwrap();
            if !state.cloud_principal_live(
                query.koru_identity_id,
                query.authenticated_by,
                query.session_id,
                query.device_id,
                query.now_millis,
            ) {
                return Err(CloudSelectionError::PermissionDenied);
            }
            let rows: Vec<CloudWorkspaceCandidate> = state
                .cloud_members
                .iter()
                .filter(|((profile_id, workspace_id), _)| {
                    query.after.is_none_or(|after| {
                        (*profile_id, *workspace_id) > (after.profile_id, after.workspace_id)
                    }) && state.cloud_profile_edge(
                        query.koru_identity_id,
                        query.authenticated_by,
                        *profile_id,
                    ) && state.cloud_eligible(*profile_id, *workspace_id)
                        && state.device_profiles.get(&(query.device_id, *profile_id))
                            != Some(&DeviceProfileState::Revoked)
                })
                .map(|((profile_id, workspace_id), _)| {
                    let workspace = state.cloud_workspaces[workspace_id];
                    CloudWorkspaceCandidate {
                        profile_id: *profile_id,
                        workspace_id: *workspace_id,
                        workspace_incarnation_id: workspace.incarnation_id,
                        authorization_version: workspace.authorization_version,
                        membership_epoch: workspace.membership_epoch,
                        cloud_access_epoch: workspace.cloud_access_epoch,
                    }
                })
                .take(CLOUD_WORKSPACE_PAGE_SIZE + 1)
                .collect();
            let has_more = rows.len() > CLOUD_WORKSPACE_PAGE_SIZE;
            let workspaces: Vec<_> = rows.into_iter().take(CLOUD_WORKSPACE_PAGE_SIZE).collect();
            let next =
                has_more
                    .then(|| workspaces.last())
                    .flatten()
                    .map(|last| CloudWorkspaceCursor {
                        profile_id: last.profile_id,
                        workspace_id: last.workspace_id,
                    });
            Ok(CloudWorkspacePage { workspaces, next })
        })
    }

    fn select_profile_cloud<'a>(
        &'a self,
        request: koru_account_core::cloud_selection::SelectProfileCloud,
        clock: &'a dyn koru_account_core::cloud_selection::SelectionClock,
    ) -> PortFuture<
        'a,
        Result<
            koru_account_core::cloud_selection::CloudSelectionOutcome,
            koru_account_core::cloud_selection::CloudSelectionError,
        >,
    > {
        use koru_account_core::cloud_selection::*;
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let principal = |state: &State, now_millis: i64| {
                state.cloud_principal_live(
                    request.koru_identity_id,
                    request.authenticated_by,
                    request.session_id,
                    request.device_id,
                    now_millis,
                )
            };
            if !principal(&state, request.now_millis)
                || !state.cloud_profile_edge(
                    request.koru_identity_id,
                    request.authenticated_by,
                    request.profile_id,
                )
            {
                return Err(CloudSelectionError::PermissionDenied);
            }
            let now_millis = clock.now_millis();
            if !principal(&state, now_millis) {
                return Err(CloudSelectionError::PermissionDenied);
            }
            let key = (request.device_id, request.profile_id);
            match state.device_profiles.get(&key) {
                Some(DeviceProfileState::Revoked) => {
                    return Err(CloudSelectionError::PermissionDenied);
                }
                Some(DeviceProfileState::Provisioned) => {
                    return Ok(CloudSelectionOutcome::AlreadySelected);
                }
                Some(DeviceProfileState::Authorized)
                | Some(DeviceProfileState::PendingProvisioning)
                | None => {}
            }
            state
                .device_profiles
                .insert(key, DeviceProfileState::Provisioned);
            state.pending_enrollment.remove(&key);
            let affected: Vec<WorkspaceId> = state
                .cloud_members
                .keys()
                .filter(|(profile_id, _)| *profile_id == request.profile_id)
                .map(|(_, workspace_id)| *workspace_id)
                .collect();
            for workspace_id in affected {
                if let Some(workspace) = state.cloud_workspaces.get_mut(&workspace_id) {
                    workspace.authorization_version += 1;
                    workspace.membership_epoch += 1;
                }
            }
            Ok(CloudSelectionOutcome::Selected)
        })
    }

    fn describe_cloud_workspace<'a>(
        &'a self,
        query: koru_account_core::cloud_selection::CloudWorkspaceDescriptorQuery,
    ) -> PortFuture<
        'a,
        Result<
            koru_account_core::cloud_selection::CloudWorkspaceDescriptor,
            koru_account_core::cloud_selection::CloudSelectionError,
        >,
    > {
        Box::pin(async move { self.describe_cloud_workspace_sync(query) })
    }
}

impl Repository {
    fn describe_cloud_workspace_sync(
        &self,
        query: koru_account_core::cloud_selection::CloudWorkspaceDescriptorQuery,
    ) -> Result<
        koru_account_core::cloud_selection::CloudWorkspaceDescriptor,
        koru_account_core::cloud_selection::CloudSelectionError,
    > {
        use koru_account_core::cloud_selection::*;
        let state = self.0.lock().unwrap();
        let binding = state
            .device_profiles
            .get(&(query.device_id, query.profile_id));
        if !state.cloud_principal_live(
            query.koru_identity_id,
            query.authenticated_by,
            query.session_id,
            query.device_id,
            query.now_millis,
        ) || !state.cloud_profile_edge(
            query.koru_identity_id,
            query.authenticated_by,
            query.profile_id,
        ) || !state.cloud_eligible(query.profile_id, query.workspace_id)
            || !matches!(
                binding,
                Some(DeviceProfileState::Authorized) | Some(DeviceProfileState::Provisioned)
            )
        {
            return Err(CloudSelectionError::PermissionDenied);
        }
        let workspace = state.cloud_workspaces[&query.workspace_id];
        let (logical_replica_id, manifest_document_id) = state
            .cloud_manifests
            .get(&query.workspace_id)
            .copied()
            .ok_or(CloudSelectionError::Unavailable)?;
        Ok(CloudWorkspaceDescriptor {
            profile_id: query.profile_id,
            workspace_id: query.workspace_id,
            workspace_incarnation_id: workspace.incarnation_id,
            logical_replica_id,
            lineage_id: workspace.incarnation_id,
            manifest_document_id,
            authorization_version: workspace.authorization_version,
            membership_epoch: workspace.membership_epoch,
            cloud_access_epoch: workspace.cloud_access_epoch,
        })
    }
}

impl SessionRepository for Repository {
    fn find_session<'a>(
        &'a self,
        id: AccountSessionId,
    ) -> PortFuture<'a, Result<Option<AccountSession>, RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            state.session_reads += 1;
            if state.fail_session_reads {
                return Err(RepositoryError::Unavailable);
            }
            Ok(state.sessions.get(&id).cloned())
        })
    }
    fn insert_session<'a>(
        &'a self,
        session: AccountSession,
    ) -> PortFuture<'a, Result<(), RepositoryError>> {
        Box::pin(async move {
            self.0.lock().unwrap().sessions.insert(session.id, session);
            Ok(())
        })
    }
    fn rotate_refresh<'a>(
        &'a self,
        desired: AccountSession,
        presented: KeyedDigest,
        retry_key: KeyedDigest,
        now: i64,
    ) -> PortFuture<'a, Result<RefreshRotationOutcome, RepositoryError>> {
        Box::pin(async move {
            let mut state = self.0.lock().unwrap();
            let existing = state
                .sessions
                .get_mut(&desired.id)
                .ok_or(RepositoryError::NotFound)?;
            if existing.revoked_at_millis.is_some() {
                return Ok(RefreshRotationOutcome::ReplayedRevoked);
            }
            if now >= existing.idle_expires_at_millis || now >= existing.absolute_expires_at_millis
            {
                existing.revoked_at_millis = Some(now);
                return Ok(RefreshRotationOutcome::ExpiredRevoked);
            }
            let family = existing
                .refresh_family
                .as_ref()
                .ok_or(RepositoryError::Conflict)?;
            if family.current_digest == presented {
                *existing = desired.clone();
                return Ok(RefreshRotationOutcome::Rotated(desired));
            }
            if family.previous_digest.as_ref() == Some(&presented)
                && family.previous_retry_key_digest.as_ref() == Some(&retry_key)
                && family
                    .previous_retry_until_millis
                    .is_some_and(|until| now < until)
                && family.sealed_previous_result.is_some()
            {
                return Ok(RefreshRotationOutcome::ExactRetry(existing.clone()));
            }
            existing.revoked_at_millis = Some(now);
            Ok(RefreshRotationOutcome::ReplayedRevoked)
        })
    }
    fn revoke_session<'a>(
        &'a self,
        id: AccountSessionId,
        at: i64,
    ) -> PortFuture<'a, Result<(), RepositoryError>> {
        Box::pin(async move {
            self.0
                .lock()
                .unwrap()
                .sessions
                .get_mut(&id)
                .ok_or(RepositoryError::NotFound)?
                .revoked_at_millis = Some(at);
            Ok(())
        })
    }
}

#[derive(Clone)]
pub struct ClockFake(Arc<AtomicI64>, i64);
impl ClockFake {
    pub fn new(now: i64) -> Self {
        Self(Arc::new(AtomicI64::new(now)), 0)
    }

    // Downstream-only, like the Repository accessors above: the hosted
    // service's end-to-end harness drives time through these.
    #[cfg(feature = "test-support")]
    pub fn advancing(now: i64, step_millis: i64) -> Self {
        Self(Arc::new(AtomicI64::new(now)), step_millis)
    }

    #[cfg(feature = "test-support")]
    pub fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}
impl Clock for ClockFake {
    fn now_millis(&self) -> i64 {
        self.0.fetch_add(self.1, Ordering::SeqCst)
    }
}
pub struct EntropyFake(AtomicU64);
impl EntropyFake {
    // Shared by this crate's tests and downstream test-support consumers.
    pub fn new(seed: u8) -> Self {
        Self(AtomicU64::new(u64::from(seed)))
    }
}
impl Entropy for EntropyFake {
    fn fill(&self, output: &mut [u8]) -> Result<(), CryptoError> {
        // The old byte counter repeated UUIDs after only 256 emitted bytes.
        // Multi-step native flows need deterministic entropy without collisions.
        for chunk in output.chunks_mut(32) {
            let counter = self.0.fetch_add(1, Ordering::SeqCst);
            let digest = Sha256::digest(counter.to_le_bytes());
            chunk.copy_from_slice(&digest[..chunk.len()]);
        }
        Ok(())
    }
}
pub struct CryptoFake;
impl TransientEncryption for CryptoFake {
    fn seal(&self, purpose: &'static str, value: &[u8]) -> Result<SealedValue, CryptoError> {
        let mut out = purpose.as_bytes().to_vec();
        out.push(0);
        out.extend(value);
        Ok(SealedValue(out))
    }
    fn open(&self, purpose: &'static str, value: &SealedValue) -> Result<Vec<u8>, CryptoError> {
        let prefix = [purpose.as_bytes(), &[0]].concat();
        value
            .0
            .strip_prefix(prefix.as_slice())
            .map(<[u8]>::to_vec)
            .ok_or(CryptoError::VerificationFailed)
    }
    fn digest(&self, purpose: &'static str, value: &[u8]) -> Result<KeyedDigest, CryptoError> {
        let mut hash = Sha256::new();
        hash.update(purpose);
        hash.update(value);
        Ok(KeyedDigest(hash.finalize().to_vec()))
    }
}
pub struct VerifierFake;
impl GoogleIdentityVerifier for VerifierFake {
    fn verify<'a>(
        &'a self,
        input: GoogleVerificationInput,
    ) -> PortFuture<'a, Result<VerifiedGoogleIdentity, ProviderVerificationError>> {
        Box::pin(async move {
            let raw = std::str::from_utf8(input.authorization_code.expose_secret())
                .map_err(|_| ProviderVerificationError::Rejected)?;
            let (subject, verified_email) = raw
                .strip_prefix("noemail:")
                .map_or((raw, Some("person@example.invalid".into())), |subject| {
                    (subject, None)
                });
            Ok(VerifiedGoogleIdentity {
                canonical_issuer: "https://accounts.google.com".into(),
                subject: subject.into(),
                verified_email,
                display_name_snapshot: None,
            })
        })
    }
}
pub struct SignerFake;
impl TokenSigner for SignerFake {
    fn sign_access_token(&self, claims: &AccessTokenClaims) -> Result<SignedToken, CryptoError> {
        Ok(SignedToken {
            key_id: "test".into(),
            compact: OpaqueSecret(claims.session_id.to_string().into_bytes()),
        })
    }
    fn public_jwks(&self) -> Result<String, CryptoError> {
        Ok("{\"keys\":[]}".into())
    }
}
pub struct LimitFake;
impl RateLimiter for LimitFake {
    fn check<'a>(
        &'a self,
        _: RateLimitScope,
        _: KeyedDigest,
        _: Option<KoruIdentityId>,
        _: i64,
        limit: u32,
        _: u64,
    ) -> PortFuture<'a, Result<RateLimitDecision, RepositoryError>> {
        Box::pin(async move {
            Ok(RateLimitDecision {
                allowed: true,
                remaining: limit,
                retry_after_millis: None,
            })
        })
    }
}

#[cfg(test)]
fn request(subject: &str) -> GoogleAuthenticationRequest {
    GoogleAuthenticationRequest {
        verification: GoogleVerificationInput {
            authorization_code: OpaqueSecret(subject.as_bytes().to_vec()),
            expected_nonce: OpaqueSecret(vec![1]),
            pkce_verifier: OpaqueSecret(vec![2]),
            expected_pkce_challenge: "challenge".into(),
        },
        terms_version_at_creation: "2026-09".into(),
        collision_flow: CollisionFlowContext {
            flow_id: AuthFlowId::mint(),
            state_digest: KeyedDigest(vec![3; 32]),
            source_digest: KeyedDigest(vec![4; 32]),
        },
    }
}

#[test]
fn coordinator_composes_identity_sessions_profiles_devices_and_privacy_status() {
    let repository = Repository::default();
    let clock = ClockFake::new(1_000_000);
    let entropy = EntropyFake::new(1);
    let crypto = CryptoFake;
    let verifier = VerifierFake;
    let signer = SignerFake;
    let limits = LimitFake;
    let security = SessionSecurity {
        issuer: "issuer",
        audience: "audience",
        clock: &clock,
        entropy: &entropy,
        sessions: &repository,
        flows: &repository,
        crypto: &crypto,
        signer: &signer,
        limiter: &limits,
    };
    let coordinator = AccountCoordinator::new(&repository, &verifier, &clock, &crypto, &security);

    assert_eq!(
        pollster::block_on(coordinator.authenticate_native(request("first"), registration()))
            .unwrap(),
        NativeAuthentication::WouldCreate
    );
    assert_eq!(
        pollster::block_on(coordinator.authenticate_web(request("first"))).unwrap(),
        WebAuthentication::WouldCreate
    );
    assert_eq!(repository.authentication_counts(), (0, 0, 0));
    assert_eq!(repository.discovery_counts(), (0, 0));
    assert!(repository.flows_snapshot().is_empty());
    // This direct coordinator only authenticates existing identities. Seed
    // the identity fixture; explicit hosted confirmation is tested by the API.
    let verified = pollster::block_on(verifier.verify(request("first").verification)).unwrap();
    let prepared = pollster::block_on(
        IdentityResolver::new(&repository, &verifier, &clock, &crypto).prepare_creation_resolution(
            verified.into(),
            "2026-09".into(),
            KoruIdentityId::mint(),
            AuthIdentityId::mint(),
            1_000_000,
            1_000_000,
            1_600_000,
            request("first").collision_flow,
        ),
    )
    .unwrap();
    pollster::block_on(
        repository
            .create_identity_with_auth(prepared.identity.unwrap(), prepared.auth_identity.unwrap()),
    )
    .unwrap();
    let created =
        pollster::block_on(coordinator.authenticate_native(request("first"), registration()))
            .unwrap();
    let created = match created {
        NativeAuthentication::Returning {
            resolved,
            device: DeviceRegistrationOutcome::Registered(_),
            ..
        } => resolved,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(repository.0.lock().unwrap().sessions.len(), 1);
    assert_eq!(repository.0.lock().unwrap().devices.len(), 1);

    let returning =
        pollster::block_on(coordinator.authenticate_native(request("first"), registration()))
            .unwrap();
    let returning = match returning {
        NativeAuthentication::Returning {
            resolved,
            device: DeviceRegistrationOutcome::AlreadyRegistered(_),
            ..
        } => resolved,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(created.identity.id, returning.identity.id);
    assert_eq!(repository.0.lock().unwrap().sessions.len(), 2);

    let before_collision = {
        let state = repository.0.lock().unwrap();
        (state.sessions.len(), state.devices.len())
    };
    let collision_flow = match pollster::block_on(
        coordinator.authenticate_native(request("collision"), registration()),
    )
    .unwrap()
    {
        NativeAuthentication::CollisionRequired { flow_id, .. } => flow_id,
        other => panic!("unexpected {other:?}"),
    };
    let after_collision = {
        let state = repository.0.lock().unwrap();
        (state.sessions.len(), state.devices.len())
    };
    assert_eq!(before_collision, after_collision);
    assert!(matches!(
        pollster::block_on(
            coordinator.complete_collision_link(collision_flow, &returning.recent_authentication,)
        )
        .unwrap(),
        LinkResolution::Linked(_)
    ));
    assert!(matches!(
        pollster::block_on(coordinator.authenticate_native(request("collision"), registration()))
            .unwrap(),
        NativeAuthentication::Returning { .. }
    ));

    let profile_id = ProfileId::new();
    assert!(matches!(
        pollster::block_on(coordinator.attach_profile(
            profile_id,
            Some("Desk".into()),
            Some("en-US".into()),
            None,
            &returning.recent_authentication
        ))
        .unwrap(),
        ProfileAttachOutcome::Attached(_)
    ));
    assert!(matches!(
        pollster::block_on(coordinator.attach_profile(
            profile_id,
            None,
            None,
            None,
            &returning.recent_authentication
        ))
        .unwrap(),
        ProfileAttachOutcome::AlreadyAttached(_)
    ));
    pollster::block_on(coordinator.link_google(
        request("linked").verification,
        &returning.recent_authentication,
    ))
    .unwrap();
    let linked =
        pollster::block_on(coordinator.authenticate_native(request("linked"), registration()))
            .unwrap();
    let linked = match linked {
        NativeAuthentication::Returning { resolved, .. } => resolved,
        other => panic!("unexpected {other:?}"),
    };
    assert!(matches!(
        pollster::block_on(coordinator.attach_profile(
            profile_id,
            None,
            None,
            None,
            &linked.recent_authentication
        ))
        .unwrap(),
        ProfileAttachOutcome::AuthorizationAdded(_)
    ));
    assert_eq!(
        pollster::block_on(coordinator.list_profiles(&linked.recent_authentication))
            .unwrap()
            .len(),
        1
    );

    let conflict_id = ProfileId::new();
    repository.0.lock().unwrap().profiles.insert(
        conflict_id,
        ProfileBinding {
            profile_id: conflict_id,
            koru_identity_id: KoruIdentityId::mint(),
            discovery_name_snapshot: None,
            locale_snapshot: None,
            lifecycle_state: ProfileLifecycleState::Active,
            lifecycle_revision: ProfileLifecycleRevision::INITIAL,
            lifecycle_operation_id: None,
            deleted_at_millis: None,
            created_at_millis: 1,
            updated_at_millis: 1,
        },
    );
    assert_eq!(
        pollster::block_on(coordinator.attach_profile(
            conflict_id,
            None,
            None,
            None,
            &linked.recent_authentication
        )),
        Err(AccountIntegrationError::Conflict)
    );

    let erased = KoruIdentityId::mint();
    repository.0.lock().unwrap().identities.insert(
        erased,
        KoruIdentity {
            id: erased,
            display_name: "Erased owner".into(),
            status: KoruIdentityStatus::Active,
            terms: TermsAcceptance {
                version: "v".into(),
                accepted_at_millis: 1,
            },
            created_at_millis: 1,
            updated_at_millis: 1,
        },
    );
    let rebound_id = ProfileId::new();
    assert_eq!(
        pollster::block_on(coordinator.rebind_erased_profile(
            erased,
            rebound_id,
            None,
            None,
            true,
            &linked.recent_authentication
        )),
        Err(AccountIntegrationError::Rebind(
            ProfileRebindError::OldIdentityStillLive
        ))
    );
    repository.0.lock().unwrap().identities.remove(&erased);
    assert_eq!(
        pollster::block_on(coordinator.rebind_erased_profile(
            erased,
            rebound_id,
            None,
            None,
            false,
            &linked.recent_authentication
        )),
        Err(AccountIntegrationError::Rebind(
            ProfileRebindError::IntentRequired
        ))
    );
    assert!(matches!(
        pollster::block_on(coordinator.rebind_erased_profile(
            erased,
            rebound_id,
            None,
            None,
            true,
            &linked.recent_authentication
        ))
        .unwrap(),
        ProfileRebindOutcome::Rebound(_)
    ));

    let revoked = pollster::block_on(coordinator.revoke_device(
        registration().descriptor.device_id,
        &linked.recent_authentication,
    ))
    .unwrap();
    assert_eq!(
        revoked.registration.revoked_at_millis,
        Some(clock.now_millis())
    );
    assert_eq!(
        pollster::block_on(
            coordinator.register_device(registration(), &linked.recent_authentication)
        ),
        Err(AccountIntegrationError::Conflict)
    );

    let status = pollster::block_on(coordinator.status(&linked.recent_authentication));
    assert_eq!(status.stage, AccountStatusStage::Ready);
    assert_eq!(status.failure, None);
    assert_eq!(status.active_device_count, 0);

    let sessions_before_conflict = repository.0.lock().unwrap().sessions.len();
    assert_eq!(
        pollster::block_on(
            coordinator.authenticate_native(request("noemail:other"), registration())
        ),
        Ok(NativeAuthentication::WouldCreate)
    );
    assert_eq!(
        repository.0.lock().unwrap().sessions.len(),
        sessions_before_conflict
    );
    clock.0.store(
        returning.recent_authentication.expires_at_millis(),
        Ordering::SeqCst,
    );
    let failed = pollster::block_on(coordinator.status(&returning.recent_authentication));
    assert_eq!(
        failed.failure,
        Some(AccountStatusFailureCode::Unauthenticated)
    );
}
