// SPDX-License-Identifier: MPL-2.0

//! The per-Profile Cloud sync toggle.
//!
//! A person turns Cloud sync on for a *Profile*. The hosted contract is
//! per *Workspace* (`workspace-cloud-lifecycle-v1`), so this module is the
//! join: it remembers the choice on this device, and applies it to every
//! Workspace the Profile owns here — now, and again for a Workspace created
//! afterwards, without the person going back to the switch.
//!
//! Three rules shape everything below.
//!
//! **One Workspace's refusal is that Workspace's refusal.** A Profile with
//! eight Workspaces where the hosted account refuses one must still get the
//! other seven. Every hosted call therefore records its outcome against one
//! Workspace and the loop continues; nothing here returns `Err` once the set
//! of Profiles is known.
//!
//! **The remembered choice is written before it is applied.** A crash between
//! the switch and the last activation must not lose the answer to "did they
//! ask for this?", because that answer is what the reconcile pass replays.
//!
//! **What is remembered is content-free** — ids, version numbers, an
//! incarnation and a stable failure code. No name, no path, no token, and
//! never the device signing key.

use std::collections::BTreeMap;
use std::sync::Arc;

use koru_account_core::workspace_lifecycle::WorkspaceCloudState;
use koru_account_core::{KoruIdentityId, ProfileId, WorkspaceId};
use koru_account_wire::{
    BeginWorkspaceCloudActivationRequestV1, CloudWorkspaceCandidateV1, CloudWorkspaceCursorV1,
    CompleteWorkspaceCloudActivationRequestV1, MutateWorkspaceCloudRequestV1, Version1,
    WorkspaceCloudLifecycleResponseV1,
};
use koru_agent_wire::{AccountCommand, AccountOperationResult, AccountStage, CloudSyncProfile};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    Cancel, CredentialChange, ProductionAccountService, ServiceError, ServiceOutcome,
    map_client_error,
};
use koru_account_client::ClientError;

/// A host that did not supply [`CloudSyncPort`] cannot honour the switch, and
/// says so rather than reporting a choice it never applied.
pub const CLOUD_SYNC_UNAVAILABLE: &str = "cloud_sync_unavailable";
/// The hosted row is at a version this device did not expect, twice in a row
/// after reading it, or a disable has no incarnation to quote. Recorded
/// against that Workspace; the rest of the Profile still applies.
const VERSION_MISMATCH: &str = "workspace_version_mismatch";
/// A challenge answered for a different Workspace than the one asked about.
/// Refusing here keeps a device signature off bytes minted for another row.
const DATASET_MISMATCH: &str = "cloud_dataset_mismatch";

/// Discovery is a position, never an account-wide snapshot: one apply reads at
/// most this many eligibility pairs and stops. A larger catalog resolves over
/// later passes rather than building an unbounded set in memory.
const MAX_LISTED_CANDIDATES: usize = 256;
/// A page is bounded by the host; this only stops a cursor that never ends.
const MAX_LISTED_PAGES: usize = 8;

/// What the runtime needs from its host before the switch can do anything.
///
/// It is optional in [`super::AccountPorts`]: a host that does not supply it —
/// Android today, and every test that does not arm it — answers
/// [`CLOUD_SYNC_UNAVAILABLE`] rather than pretending a choice was applied.
///
/// **Signing is deliberately not here.** The service already holds this
/// device's registered signing key — it is the same key, loaded from the same
/// place, that signs enrollment challenges — and handing a second copy across
/// a port would put the key in one more place for no new capability.
pub trait CloudSyncPort: Send + Sync {
    /// Every Workspace this device holds locally, with the Profile that owns
    /// it. One call per command rather than one per Profile: on the desktop
    /// this is a single read of the Store's `workspaces` table.
    fn workspaces(&self) -> Result<Vec<(ProfileId, WorkspaceId)>, ServiceError>;

    /// The remembered state, or [`DeviceCloudSync::default`] when this device
    /// has never written one.
    fn load(&self) -> Result<DeviceCloudSync, ServiceError>;

    /// Replace the remembered state. Called only when something changed.
    fn store(&self, state: &DeviceCloudSync) -> Result<(), ServiceError>;

    /// Whether this host has a Store it must open to Cloud
    /// ([`Self::open_to_cloud`]) — the desktop does. Where it does, a Profile
    /// whose Cloud sync is on is opened by the reconcile, and a Profile
    /// created with the account or chosen from it starts on by itself
    /// (ADR-000-createdwithaccount). `false` leaves a host exactly as it
    /// was: nothing is opened and nothing starts unasked.
    fn starts_created_profiles_on(&self) -> bool {
        false
    }

    /// **The device-local half of "on"**: let this device's Store move
    /// `profile` and its Workspaces through Koru Cloud — the transport mode
    /// (`Store::choose_transport_mode`, the Profile and its Workspaces as
    /// one group). Asked once per process for every Profile whose Cloud sync
    /// is on, before the hosted half, so it must be safe to repeat.
    fn open_to_cloud(&self, _profile: ProfileId) -> Result<(), ServiceError> {
        Err(ServiceError::terminal(CLOUD_SYNC_UNAVAILABLE))
    }
}

/// The on-disk shape's version. A file written by a newer build is not read by
/// an older one: an unknown version starts from the default rather than being
/// half-understood, which costs one re-activation and never a wrong disable.
pub const STATE_VERSION: u32 = 1;

/// This device's whole Cloud sync memory, across every account signed in on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCloudSync {
    pub version: u32,
    /// Keyed by the account's Koru identity id. Profile ids are unique
    /// globally, but a second account's Profiles must not be answered to the
    /// first, so the account is part of the key rather than an assumption.
    #[serde(default)]
    pub accounts: BTreeMap<String, AccountCloudSync>,
}

impl Default for DeviceCloudSync {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            accounts: BTreeMap::new(),
        }
    }
}

impl DeviceCloudSync {
    /// A file from a version this build does not understand is discarded, not
    /// guessed at. Hosts call this after decoding.
    pub fn accepted(self) -> Self {
        if self.version == STATE_VERSION {
            self
        } else {
            Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountCloudSync {
    #[serde(default)]
    pub profiles: BTreeMap<ProfileId, ProfileCloudSync>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileCloudSync {
    /// The remembered answer to the switch. It is what the reconcile pass
    /// replays, so it outlives every individual Workspace's outcome.
    pub enabled: bool,
    /// **Created with the account on this device, and its switch not yet
    /// touched** (ADR-000-createdwithaccount): Cloud sync starts on by
    /// itself, as soon as the account has Koru Cloud. Written when the
    /// account's Profile is created, cleared by the pass that leaves every
    /// Workspace syncing, and by a person moving the switch either way —
    /// their answer replaces the default. Never set for a Profile that
    /// existed before it was connected: that one is still asked (D258).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub starts_on: bool,
    #[serde(default)]
    pub workspaces: BTreeMap<WorkspaceId, WorkspaceCloudSync>,
}

/// What this device knows about one Workspace's hosted Cloud row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCloudSync {
    /// Whether this device believes the hosted row is enabled right now.
    pub enabled: bool,
    /// The authorization version last seen. Quoted as
    /// `expected_authorization_version`; `0` means no hosted row was ever seen.
    #[serde(default)]
    pub authorization_version: u64,
    /// `None` until an activation committed. Disable quotes it and cannot
    /// invent one.
    #[serde(default)]
    pub incarnation_id: Option<Uuid>,
    /// Whether this device has read the hosted row at least once. Until it
    /// has, `authorization_version == 0` means "unknown", not "no row".
    #[serde(default)]
    pub observed: bool,
    /// The stable code of the last refusal, never a message with content in it.
    #[serde(default)]
    pub error: Option<String>,
}

impl WorkspaceCloudSync {
    fn commit(&mut self, response: &WorkspaceCloudLifecycleResponseV1, enabled: bool) {
        self.enabled = enabled;
        self.authorization_version = response.authorization_version;
        self.incarnation_id = Some(response.incarnation_id);
        self.observed = true;
        self.error = None;
    }
}

/// One apply's view of the account's enabled Workspaces, fetched at most once
/// and only when something actually needs it.
struct Listed {
    candidates: Option<Vec<CloudWorkspaceCandidateV1>>,
    /// Why the list could not be read, if it could not. A caller that was
    /// relying on it reports this instead of blaming what it was checking.
    failure: Option<&'static str>,
}

impl Listed {
    fn new() -> Self {
        Self {
            candidates: None,
            failure: None,
        }
    }

    fn get(
        &mut self,
        service: &ProductionAccountService,
        cancel: &Cancel,
    ) -> &[CloudWorkspaceCandidateV1] {
        if self.candidates.is_none() {
            self.candidates = Some(match service.list_cloud_workspaces(cancel) {
                Ok(candidates) => candidates,
                Err(error) => {
                    self.failure = Some(error.code);
                    Vec::new()
                }
            });
        }
        self.candidates.as_deref().unwrap_or_default()
    }

    fn find(
        &mut self,
        service: &ProductionAccountService,
        cancel: &Cancel,
        profile: ProfileId,
        workspace: WorkspaceId,
    ) -> Option<CloudWorkspaceCandidateV1> {
        self.get(service, cancel)
            .iter()
            .find(|candidate| {
                candidate.profile_id == profile && candidate.workspace_id == workspace
            })
            .copied()
    }
}

/// `authorization_version` crosses the discovery page as `i64` and the
/// lifecycle request as `u64`. A negative value is a host that broke its own
/// contract; treating it as "unknown" is the only safe reading.
fn listed_version(candidate: &CloudWorkspaceCandidateV1) -> u64 {
    u64::try_from(candidate.authorization_version).unwrap_or(0)
}

fn client_error_code(error: &ClientError) -> Option<&str> {
    match error {
        ClientError::Api { code, .. } => Some(code.as_str()),
        _ => None,
    }
}

impl ProductionAccountService {
    /// Local policy must be visible before any hosted inspection can finish.
    /// This runs on the account worker, never on a UI or snapshot-reader thread.
    pub(super) fn cloud_profile_choices(
        &self,
        account: KoruIdentityId,
    ) -> Vec<koru_agent_wire::AccountProfileCloud> {
        let Some(state) = self.cloud_sync.as_ref().and_then(|port| port.load().ok()) else {
            return Vec::new();
        };
        state
            .accepted()
            .accounts
            .get(&account.to_string())
            .into_iter()
            .flat_map(|account| &account.profiles)
            .filter(|(_, profile)| profile.enabled)
            .map(|(id, _)| koru_agent_wire::AccountProfileCloud {
                discovery: None,
                profile_id: id.to_string(),
                stage: koru_agent_wire::CloudConnectionStage::Connecting,
            })
            .collect()
    }

    /// **A Profile created with the account starts with Cloud sync on**
    /// (ADR-000-createdwithaccount). Called once the account's Profile
    /// exists; what is written is only the intention, and the reconcile pass
    /// acts on it once the Profile is bound on this device and the account
    /// has Koru Cloud.
    ///
    /// Best effort on purpose: the Profile is already created, and a state
    /// file that cannot be written costs a person one press of the switch,
    /// never the Profile. A Profile this device already has an answer or a
    /// hosted row for is left alone — a retried creation must not undo a
    /// switch somebody has since turned off.
    pub(super) fn cloud_sync_starts_on(&self, account: KoruIdentityId, profile: ProfileId) {
        let Some(port) = self
            .cloud_sync
            .as_ref()
            .filter(|port| port.starts_created_profiles_on())
        else {
            return;
        };
        let Ok(state) = port.load() else {
            return;
        };
        let mut state = state.accepted();
        let entry = state
            .accounts
            .entry(account.to_string())
            .or_default()
            .profiles
            .entry(profile)
            .or_default();
        if entry.enabled || entry.starts_on || !entry.workspaces.is_empty() {
            return;
        }
        entry.starts_on = true;
        let _ = port.store(&state);
    }

    /// Whether `account` has Koru Cloud, for a Profile waiting to start.
    ///
    /// The settings page's own question (`InspectCloudSubscription`) is
    /// remembered, so the usual pass costs nothing. Only an Agent nobody has
    /// asked yet reads it here, once: a `false` is kept until the page asks
    /// again, so an account with no access is not polled on the
    /// heartbeat. A read that failed is not an answer and is asked again.
    fn cloud_access_known(&mut self, account: KoruIdentityId, cancel: &Cancel) -> Option<bool> {
        if let Some((known, current)) = self.cloud_access
            && known == account
        {
            return Some(current);
        }
        let authorization = self.authorization().ok()?;
        let current = self
            .client
            .cloud_subscription_status(&authorization, &|| cancel.cancelled())
            .ok()?
            .cloud_access;
        self.cloud_access = Some((account, current));
        Some(current)
    }

    fn cloud_sync_port(&self) -> Result<Arc<dyn CloudSyncPort>, ServiceError> {
        self.cloud_sync
            .clone()
            .ok_or_else(|| ServiceError::terminal(CLOUD_SYNC_UNAVAILABLE))
    }

    fn cloud_sync_account(&self) -> Result<KoruIdentityId, ServiceError> {
        self.durable_owner
            .map(|owner| owner.account)
            .ok_or_else(|| ServiceError::terminal("credential_owner_unavailable"))
    }

    /// Every Profile of the current account whose content is still present on
    /// this device, in a stable order.
    fn cloud_sync_profiles(&self, account: KoruIdentityId) -> Result<Vec<ProfileId>, ServiceError> {
        let binding = format!("koru:{account}");
        let mut profiles: Vec<ProfileId> = self
            .local_profiles
            .inventory()?
            .profiles
            .into_iter()
            .filter(|item| item.account_binding == binding && item.removed_at_millis.is_none())
            .map(|item| item.profile_id)
            .collect();
        profiles.sort();
        profiles.dedup();
        Ok(profiles)
    }

    fn list_cloud_workspaces(
        &self,
        cancel: &Cancel,
    ) -> Result<Vec<CloudWorkspaceCandidateV1>, ServiceError> {
        let authorization = self.authorization()?;
        let mut all: Vec<CloudWorkspaceCandidateV1> = Vec::new();
        let mut after: Option<CloudWorkspaceCursorV1> = None;
        for _ in 0..MAX_LISTED_PAGES {
            if cancel.cancelled() {
                break;
            }
            let page = self
                .client
                .cloud_workspaces(after.as_ref(), &authorization)
                .map_err(map_client_error)?;
            all.extend(page.workspaces);
            match page.next {
                Some(cursor) if all.len() < MAX_LISTED_CANDIDATES => after = Some(cursor),
                _ => break,
            }
        }
        all.truncate(MAX_LISTED_CANDIDATES);
        Ok(all)
    }

    fn mint_operation_id(&self) -> Uuid {
        let mut bytes = [0_u8; 16];
        self.entropy.fill_bytes(&mut bytes);
        uuid::Builder::from_random_bytes(bytes).into_uuid()
    }

    /// Bring this device's knowledge of one Workspace's hosted row up to
    /// date. The hosted row is the truth; what this device remembers is only
    /// a cache of it, so a switch on another device is seen here rather than
    /// fought. Cheap on purpose: the account's enabled list (one call per
    /// command) answers every enabled row, and a row is read on its own only
    /// when this device believed it enabled and the list no longer says so,
    /// or when this device has never seen it at all.
    fn observe_workspace(
        &self,
        profile: ProfileId,
        workspace: WorkspaceId,
        entry: &mut WorkspaceCloudSync,
        listed: &mut Listed,
        cancel: &Cancel,
    ) -> Option<String> {
        if let Some(candidate) = listed.find(self, cancel, profile, workspace) {
            entry.enabled = true;
            entry.authorization_version = listed_version(&candidate);
            entry.incarnation_id = Some(candidate.workspace_incarnation_id);
            entry.observed = true;
            entry.error = None;
            return None;
        }
        if !entry.enabled && entry.observed {
            // Known off or known absent, and the list did not change that.
            return None;
        }
        let authorization = match self.authorization() {
            Ok(authorization) => authorization,
            Err(error) => return Some(error.code.into()),
        };
        match self
            .client
            .inspect_workspace_cloud(workspace, &authorization)
        {
            Ok(response) => {
                entry.enabled = response.state == WorkspaceCloudState::Enabled;
                entry.authorization_version = response.authorization_version;
                entry.incarnation_id =
                    (!response.incarnation_id.is_nil()).then_some(response.incarnation_id);
                entry.observed = true;
                entry.error = None;
                None
            }
            Err(error) => Some(
                client_error_code(&error)
                    .map(str::to_owned)
                    .unwrap_or_else(|| map_client_error(error).code.into()),
            ),
        }
    }

    /// Turn Cloud sync on for one Workspace. Returns the failure code to
    /// record, or `None` when the Workspace is now enabled.
    fn activate_workspace(
        &self,
        profile: ProfileId,
        workspace: WorkspaceId,
        entry: &mut WorkspaceCloudSync,
        listed: &mut Listed,
        cancel: &Cancel,
    ) -> Option<String> {
        // The version quoted below must be the current one, so the row is
        // read first when this device's knowledge could be stale.
        if let Some(code) = self.observe_workspace(profile, workspace, entry, listed, cancel) {
            return Some(code);
        }
        if entry.enabled {
            return None;
        }
        let authorization = match self.authorization() {
            Ok(authorization) => authorization,
            Err(error) => return Some(error.code.into()),
        };
        let begin = |expected_authorization_version: u64| {
            self.client.begin_workspace_cloud_activation(
                workspace,
                &BeginWorkspaceCloudActivationRequestV1 {
                    version: Version1,
                    operation_id: self.mint_operation_id(),
                    owner_profile_id: profile,
                    expected_authorization_version,
                },
                &authorization,
            )
        };
        let challenge = match begin(entry.authorization_version) {
            Ok(challenge) => challenge,
            Err(error) if client_error_code(&error) == Some(VERSION_MISMATCH) => {
                // Another device moved the row between the read and this
                // begin. Read once more and try once more; a second mismatch
                // is reported as what it is.
                entry.observed = false;
                if let Some(code) =
                    self.observe_workspace(profile, workspace, entry, listed, cancel)
                {
                    return Some(code);
                }
                if entry.enabled {
                    return None;
                }
                match begin(entry.authorization_version) {
                    Ok(challenge) => challenge,
                    Err(error) => {
                        return Some(
                            client_error_code(&error)
                                .map(str::to_owned)
                                .unwrap_or_else(|| map_client_error(error).code.into()),
                        );
                    }
                }
            }
            Err(error) => {
                return Some(
                    client_error_code(&error)
                        .map(str::to_owned)
                        .unwrap_or_else(|| map_client_error(error).code.into()),
                );
            }
        };
        if challenge.workspace_id != workspace {
            return Some(DATASET_MISMATCH.into());
        }
        let signature = self.signing_key.sign_message(&challenge.signing_bytes);
        match self.client.complete_workspace_cloud_activation(
            workspace,
            &CompleteWorkspaceCloudActivationRequestV1 {
                version: Version1,
                challenge_id: challenge.challenge_id,
                signature: signature.as_bytes().to_vec(),
            },
            &authorization,
        ) {
            Ok(response) => {
                entry.commit(&response, true);
                None
            }
            Err(error) => Some(
                client_error_code(&error)
                    .map(str::to_owned)
                    .unwrap_or_else(|| map_client_error(error).code.into()),
            ),
        }
    }

    /// Turn Cloud sync off for one Workspace, quoting the version and
    /// incarnation the hosted row holds right now.
    fn disable_workspace(
        &self,
        profile: ProfileId,
        workspace: WorkspaceId,
        entry: &mut WorkspaceCloudSync,
        listed: &mut Listed,
        cancel: &Cancel,
    ) -> Option<String> {
        if let Some(code) = self.observe_workspace(profile, workspace, entry, listed, cancel) {
            return Some(code);
        }
        if !entry.enabled {
            return None;
        }
        let authorization = match self.authorization() {
            Ok(authorization) => authorization,
            Err(error) => return Some(error.code.into()),
        };
        // Disable quotes an incarnation. Without one there is nothing honest
        // to send.
        let Some(expected_incarnation_id) = entry.incarnation_id else {
            return Some(VERSION_MISMATCH.into());
        };
        match self.client.disable_workspace_cloud(
            workspace,
            &MutateWorkspaceCloudRequestV1 {
                version: Version1,
                operation_id: self.mint_operation_id(),
                owner_profile_id: profile,
                expected_authorization_version: entry.authorization_version,
                expected_incarnation_id,
            },
            &authorization,
        ) {
            Ok(response) => {
                entry.commit(&response, false);
                None
            }
            Err(error) => Some(
                client_error_code(&error)
                    .map(str::to_owned)
                    .unwrap_or_else(|| map_client_error(error).code.into()),
            ),
        }
    }

    /// Bring one Profile to what was asked of it and describe the result.
    ///
    /// `change` is the switch this command moved, if it moved this Profile's:
    /// `Some(true)` activates every Workspace this device holds for it,
    /// including one another device turned off; `Some(false)` disables every
    /// enabled one. `None` is the reconcile pass, which **never disables**: it
    /// refreshes what this device knows, mirrors the hosted truth into the
    /// Profile's switch, and activates only a Workspace the Cloud has never
    /// held (one created after the switch), so another device's "off" stays
    /// off and another device's "on" shows as on here.
    ///
    /// `persist` is called after every hosted mutation, so a crash or a later
    /// refusal never loses a version the row now expects.
    #[allow(clippy::too_many_arguments)]
    fn apply_profile(
        &self,
        profile: ProfileId,
        state: &mut ProfileCloudSync,
        local: &[WorkspaceId],
        change: Option<bool>,
        by_default: bool,
        listed: &mut Listed,
        cancel: &Cancel,
        woken: &mut bool,
        persist: &mut dyn FnMut(ProfileId, &ProfileCloudSync),
    ) -> CloudSyncProfile {
        let mut last_error: Option<String> = None;
        let mut record = |entry: &mut WorkspaceCloudSync, code: Option<String>| {
            if let Some(code) = code {
                entry.error = Some(code.clone());
                last_error = Some(code);
            }
        };
        match change {
            Some(true) => {
                for workspace in local {
                    if cancel.cancelled() {
                        break;
                    }
                    let entry = state.workspaces.entry(*workspace).or_default();
                    // **A default never undoes somebody's "off".** A Profile
                    // that starts by itself activates what the Cloud has
                    // never held and leaves alone a Workspace another device
                    // turned off; only a pressed switch turns that back on.
                    if by_default {
                        let code =
                            self.observe_workspace(profile, *workspace, entry, listed, cancel);
                        let off_elsewhere =
                            code.is_none() && !entry.enabled && entry.authorization_version > 0;
                        if code.is_some() || off_elsewhere {
                            record(entry, code);
                            continue;
                        }
                    }
                    match self.activate_workspace(profile, *workspace, entry, listed, cancel) {
                        None => *woken = true,
                        code => record(entry, code),
                    }
                    persist(profile, state);
                }
            }
            Some(false) => {
                let mut targets: Vec<WorkspaceId> = local.to_vec();
                targets.extend(
                    state
                        .workspaces
                        .iter()
                        .filter(|(_, entry)| entry.enabled)
                        .map(|(workspace, _)| *workspace),
                );
                targets.sort();
                targets.dedup();
                for workspace in targets {
                    if cancel.cancelled() {
                        break;
                    }
                    let entry = state.workspaces.entry(workspace).or_default();
                    let code = self.disable_workspace(profile, workspace, entry, listed, cancel);
                    record(entry, code);
                    persist(profile, state);
                }
            }
            None => {
                for workspace in local {
                    if cancel.cancelled() {
                        break;
                    }
                    let entry = state.workspaces.entry(*workspace).or_default();
                    let code = self.observe_workspace(profile, *workspace, entry, listed, cancel);
                    record(entry, code);
                }
                let known = |workspace: &WorkspaceId| state.workspaces.get(workspace);
                let any_enabled = local.iter().any(|w| known(w).is_some_and(|e| e.enabled));
                let all_observed = local.iter().all(|w| known(w).is_some_and(|e| e.observed));
                if any_enabled {
                    state.enabled = true;
                } else if all_observed && !local.is_empty() {
                    state.enabled = false;
                }
                if state.enabled {
                    for workspace in local {
                        if cancel.cancelled() {
                            break;
                        }
                        let entry = state.workspaces.entry(*workspace).or_default();
                        let never_hosted =
                            !entry.enabled && entry.observed && entry.authorization_version == 0;
                        if !never_hosted {
                            continue;
                        }
                        match self.activate_workspace(profile, *workspace, entry, listed, cancel) {
                            None => *woken = true,
                            code => record(entry, code),
                        }
                        persist(profile, state);
                    }
                }
            }
        }
        let synced = local
            .iter()
            .filter(|workspace| {
                state
                    .workspaces
                    .get(*workspace)
                    .is_some_and(|entry| entry.enabled)
            })
            .count();
        CloudSyncProfile {
            profile_id: profile.to_storage(),
            enabled: state.enabled,
            workspaces: u32::try_from(local.len()).unwrap_or(u32::MAX),
            synced_workspaces: u32::try_from(synced).unwrap_or(u32::MAX),
            error: last_error,
        }
    }

    /// Both Cloud sync commands. `InspectCloudSync` reports every local
    /// Profile against the hosted truth and activates what the Cloud has never
    /// held for a Profile that is on — which is how a Workspace created after
    /// the switch reaches the Cloud without anyone going back to it. It never
    /// turns anything off: only a person's explicit switch does.
    pub(super) fn cloud_sync(
        &mut self,
        generation: u64,
        command: AccountCommand,
        cancel: &Cancel,
    ) -> Result<ServiceOutcome, ServiceError> {
        let (request_id, change) = match &command {
            AccountCommand::InspectCloudSync { request_id } => (*request_id, None),
            AccountCommand::SetProfileCloudSync {
                request_id,
                profile_id,
                enabled,
            } => (
                *request_id,
                Some((super::parse_profile(profile_id)?, *enabled)),
            ),
            _ => return Err(ServiceError::terminal("invalid_request")),
        };
        let port = self.cloud_sync_port()?;
        let account = self.cloud_sync_account()?;
        let profiles = self.cloud_sync_profiles(account)?;
        if let Some((profile, _)) = change
            && !profiles.contains(&profile)
        {
            return Err(ServiceError::terminal("profile_not_authorized"));
        }
        let mut local: BTreeMap<ProfileId, Vec<WorkspaceId>> = BTreeMap::new();
        for (owner, workspace) in port.workspaces()? {
            local.entry(owner).or_default().push(workspace);
        }
        for workspaces in local.values_mut() {
            workspaces.sort();
            workspaces.dedup();
        }

        let mut state = port.load()?.accepted();
        // What is on disk right now, so no write below rewrites bytes that are
        // already there: the reconcile pass runs on a timer, and a timer that
        // touches disk for no reason is a bug.
        let mut saved = state.clone();
        let account_key = account.to_string();
        // The choice is written before it is applied: a crash between the two
        // must lose at most the activation, never the answer to "did they ask
        // for this?".
        if let Some((profile, enabled)) = change {
            let chosen = state
                .accounts
                .entry(account_key.clone())
                .or_default()
                .profiles
                .entry(profile)
                .or_default();
            chosen.enabled = enabled;
            // A person's own answer replaces the default, whichever way they
            // moved the switch.
            chosen.starts_on = false;
            if state != saved {
                port.store(&state)?;
                saved = state.clone();
            }
        }

        let mut listed = Listed::new();
        let mut woken = false;
        let mut started: Vec<ProfileId> = Vec::new();
        let mut rows = Vec::with_capacity(profiles.len());
        for profile in &profiles {
            // `apply_profile` reads the service while it mutates this Profile's
            // entry, so it works on a detached copy that `persist` puts back
            // after every hosted mutation and once more at the end.
            let mut profile_state = state
                .accounts
                .entry(account_key.clone())
                .or_default()
                .profiles
                .entry(*profile)
                .or_default()
                .clone();
            let workspaces = local.get(profile).cloned().unwrap_or_default();
            let mut applied =
                change.and_then(|(changed, enabled)| (changed == *profile).then_some(enabled));
            // **A Profile created with the account, or chosen from it,
            // starts on** (ADR-000-createdwithaccount) — once the account
            // has Koru Cloud and this device's Store is open to it. The
            // Store first: until it is open nothing of the Profile's can
            // arrive here, and the hosted half must never run with every
            // file held back. It is opened once, before any Workspace is
            // here, because a Profile chosen from the account has none
            // until it is. Anything missing leaves the intention for the
            // next pass.
            let waiting = applied.is_none()
                && profile_state.starts_on
                && self.cloud_access_known(account, cancel) == Some(true);
            // **Cloud sync on means the Store is open to Cloud.** One rule,
            // however the Profile came to be on: the switch, the default, or
            // another device's switch mirrored here. The two used to be
            // separate acts — the switch, then a question whose answer opened
            // the Store — and a switch left on with the question unanswered
            // synced nothing while reading *Koru Cloud unavailable*
            // (2026-11-11). Once per process, since opening an open Store
            // writes nothing.
            let on =
                applied == Some(true) || waiting || (applied.is_none() && profile_state.enabled);
            let gated = port.starts_created_profiles_on();
            if gated
                && on
                && !self.cloud_opened.contains(profile)
                && port.open_to_cloud(*profile).is_ok()
            {
                self.cloud_opened.insert(*profile);
                // The Cloud pass was being refused until this moment.
                woken = true;
            }
            let open = !gated || self.cloud_opened.contains(profile);
            let starting = waiting && open;
            let opened = starting && !profile_state.enabled;
            if opened {
                profile_state.enabled = true;
                started.push(*profile);
            }
            if starting {
                applied = Some(true);
            }
            let mut persist = |profile: ProfileId, current: &ProfileCloudSync| {
                state
                    .accounts
                    .entry(account_key.clone())
                    .or_default()
                    .profiles
                    .insert(profile, current.clone());
                if state != saved && port.store(&state).is_ok() {
                    saved = state.clone();
                }
            };
            if opened {
                // Written before it is applied, as a pressed switch is.
                persist(*profile, &profile_state);
            }
            let row = self.apply_profile(
                *profile,
                &mut profile_state,
                &workspaces,
                applied,
                starting,
                &mut listed,
                cancel,
                &mut woken,
                &mut persist,
            );
            // The intention is spent by the first pass that settled every
            // Workspace the Profile has here. Until one has arrived there is
            // nothing to settle; a refusal or an outage tries again on the
            // next pass, as a Workspace the Cloud has never held does.
            if starting && row.error.is_none() && row.workspaces > 0 && !cancel.cancelled() {
                profile_state.starts_on = false;
            }
            persist(*profile, &profile_state);
            rows.push(row);
        }
        if state != saved {
            port.store(&state)?;
        }
        // Discovery runs now rather than at the next heartbeat, and the
        // command does not wait for the pass.
        if woken && let Some(cloud) = &self.cloud {
            cloud.wake();
        }
        // A Profile that started by itself is owed what a pressed switch is.
        for profile in change
            .map(|(profile, _)| profile)
            .into_iter()
            .chain(started)
        {
            if let Some(enrollment) = &self.enrollment
                && let Some(authority) = self.enrollment_authorization()
            {
                enrollment.retry(authority, Some(profile));
            }
        }
        Ok(ServiceOutcome {
            state: self.state(generation, AccountStage::SignedIn),
            credential: CredentialChange::Keep,
            result: AccountOperationResult::CloudSync {
                request_id,
                profiles: rows,
            },
            follow_up: None,
        })
    }
}
