// SPDX-License-Identifier: MPL-2.0

//! Hosted Workspace Cloud lifecycle and its durable authority stream.
//!
//! This module contains no SQL or HTTP.  The PostgreSQL adapter owns locking
//! and transactions; callers cannot manufacture principals or completion
//! evidence through these values.

use koru_device::{DeviceId, DevicePublicKey, SignatureBytes};
use koru_store_core::{ProfileId, WorkspaceId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::workspace_authorization::{Principal, Region, WorkspaceScope};
use crate::{AccountSessionId, KoruIdentityId, PortFuture};

pub const WORKSPACE_AUTHORIZATION_SNAPSHOT_VERSION: u32 = 1;
pub const WORKSPACE_ACTIVATION_CHALLENGE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceCloudState {
    Enabled,
    Disabled,
    Deleting,
    Absent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMemberState {
    Active,
    Revoked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceCloudChangeKind {
    InitialEnable,
    Enable,
    Disable,
    Delete,
    DeleteCompleted,
    MembershipChanged,
    AuthorityChanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceCloudRecord {
    pub workspace_id: WorkspaceId,
    pub region: Region,
    /// **A transition projection since root-catalog Plan 3.1**, not
    /// ownership and not placement (TECHNICAL.final §3.2, §7.3). The
    /// account that owns the Workspace is `owner_account_id`; where the
    /// Workspace is placed is the account catalog's Workspace unit, read
    /// through [`crate::root_catalog::DiscoveryProjection`]. This column
    /// keeps the Profile the Workspace was enabled under so v1 readers and
    /// the v1 descriptor keep working during the compatibility window; no
    /// v2 admission decision reads it, and a move never rewrites it.
    pub owner_profile_id: ProfileId,
    pub owner_account_id: KoruIdentityId,
    pub state: WorkspaceCloudState,
    pub incarnation_id: Uuid,
    pub membership_epoch: u64,
    pub cloud_access_epoch: u64,
    pub authorization_version: u64,
    pub created_at_millis: i64,
    pub updated_at_millis: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceActivationChallenge {
    pub version: u32,
    pub challenge_id: Uuid,
    pub operation_id: Uuid,
    pub workspace_id: WorkspaceId,
    pub region: Region,
    pub owner_profile_id: ProfileId,
    pub owner_account_id: KoruIdentityId,
    pub device_id: DeviceId,
    pub session_id: AccountSessionId,
    pub issuer: String,
    pub registered_device_signing_public_key: DevicePublicKey,
    pub expected_authorization_version: u64,
    pub expected_profile_lifecycle_revision: u64,
    pub nonce: [u8; 32],
    pub issued_at_millis: i64,
    pub expires_at_millis: i64,
    pub consumed_at_millis: Option<i64>,
}

impl WorkspaceActivationChallenge {
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut bytes = b"koru/cloud/workspace-activation/v1\0".to_vec();
        bytes.extend_from_slice(self.challenge_id.as_bytes());
        bytes.extend_from_slice(self.operation_id.as_bytes());
        bytes.extend_from_slice(self.workspace_id.as_uuid().as_bytes());
        bytes.push(match self.region {
            Region::Us => 1,
            Region::Eu => 2,
        });
        field(&mut bytes, self.issuer.as_bytes());
        bytes.extend_from_slice(self.owner_profile_id.as_uuid().as_bytes());
        bytes.extend_from_slice(self.owner_account_id.as_uuid().as_bytes());
        bytes.extend_from_slice(self.device_id.as_uuid().as_bytes());
        bytes.extend_from_slice(self.session_id.as_uuid().as_bytes());
        bytes.extend_from_slice(self.registered_device_signing_public_key.as_bytes());
        bytes.extend_from_slice(&self.expected_authorization_version.to_be_bytes());
        bytes.extend_from_slice(&self.expected_profile_lifecycle_revision.to_be_bytes());
        bytes.extend_from_slice(&self.issued_at_millis.to_be_bytes());
        bytes.extend_from_slice(&self.expires_at_millis.to_be_bytes());
        bytes.extend_from_slice(&self.nonce);
        bytes
    }

    pub fn verifies(&self, signature: SignatureBytes) -> bool {
        self.version == WORKSPACE_ACTIVATION_CHALLENGE_VERSION
            && self
                .registered_device_signing_public_key
                .verify_message(&self.signing_bytes(), signature)
    }
}

fn field(target: &mut Vec<u8>, value: &[u8]) {
    target.extend_from_slice(&(value.len() as u64).to_be_bytes());
    target.extend_from_slice(value);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeginWorkspaceActivation {
    pub operation_id: Uuid,
    pub workspace_id: WorkspaceId,
    pub region: Region,
    pub owner_profile_id: ProfileId,
    pub owner_account_id: KoruIdentityId,
    pub device_id: DeviceId,
    pub session_id: AccountSessionId,
    pub issuer: String,
    pub expected_authorization_version: u64,
    pub now_millis: i64,
    pub challenge_id: Uuid,
    pub nonce: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompleteWorkspaceActivation {
    pub challenge_id: Uuid,
    pub workspace_id: WorkspaceId,
    pub session_id: AccountSessionId,
    pub signature: SignatureBytes,
    pub now_millis: i64,
    pub incarnation_id: Uuid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceLifecycleCommand {
    Enable,
    Disable,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MutateWorkspaceLifecycle {
    pub operation_id: Uuid,
    pub workspace_id: WorkspaceId,
    pub actor_account_id: KoruIdentityId,
    pub actor_profile_id: ProfileId,
    pub session_id: AccountSessionId,
    pub device_id: DeviceId,
    pub command: WorkspaceLifecycleCommand,
    pub expected_authorization_version: u64,
    pub expected_incarnation_id: Uuid,
    pub now_millis: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceLifecycleStatus {
    Committed,
    ExactRetry,
    AlreadyEnabled,
    AlreadyDisabled,
    AlreadyDeleting,
    AlreadyAbsent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceLifecycleOutcome {
    pub status: WorkspaceLifecycleStatus,
    pub workspace: WorkspaceCloudRecord,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceLifecycleError {
    NotAuthorized,
    InvalidChallenge,
    ChallengeExpired,
    ChallengeConsumed,
    OperationReplay,
    VersionMismatch,
    IncarnationMismatch,
    Deleting,
    Unavailable,
    Corrupt,
}

impl WorkspaceLifecycleError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotAuthorized => "workspace_not_authorized",
            Self::InvalidChallenge => "workspace_activation_invalid",
            Self::ChallengeExpired => "workspace_activation_expired",
            Self::ChallengeConsumed => "workspace_activation_consumed",
            Self::OperationReplay => "workspace_operation_replay",
            Self::VersionMismatch => "workspace_version_mismatch",
            Self::IncarnationMismatch => "cloud_incarnation_stale",
            Self::Deleting => "cloud_deleting",
            Self::Unavailable => "authorization_unavailable",
            Self::Corrupt => "authorization_corrupt",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntitlementFact {
    pub allowed: bool,
    pub version: u64,
    pub active_until_millis: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteQuotaFact {
    pub allowed: bool,
    pub version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceAuthorizationSnapshotV1 {
    pub version: u32,
    pub workspace: WorkspaceCloudRecord,
    pub principal: Principal,
    pub scopes: Vec<WorkspaceScope>,
    pub qualifying_profile_ids: Vec<ProfileId>,
    pub entitlement: EntitlementFact,
    pub write_quota: WriteQuotaFact,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceCloudChange {
    pub workspace_id: WorkspaceId,
    pub region: Region,
    pub authorization_version: u64,
    pub operation_id: Uuid,
    pub kind: WorkspaceCloudChangeKind,
    pub request_digest: [u8; 32],
    pub membership_epoch: u64,
    pub cloud_access_epoch: u64,
    pub state: WorkspaceCloudState,
    pub incarnation_id: Uuid,
    pub committed_at_millis: i64,
}

pub trait WorkspaceCloudLifecycleRepository: Send + Sync {
    fn begin_activation<'a>(
        &'a self,
        request: BeginWorkspaceActivation,
    ) -> PortFuture<'a, Result<WorkspaceActivationChallenge, WorkspaceLifecycleError>>;

    fn complete_activation<'a>(
        &'a self,
        request: CompleteWorkspaceActivation,
    ) -> PortFuture<'a, Result<WorkspaceLifecycleOutcome, WorkspaceLifecycleError>>;

    fn mutate_lifecycle<'a>(
        &'a self,
        request: MutateWorkspaceLifecycle,
    ) -> PortFuture<'a, Result<WorkspaceLifecycleOutcome, WorkspaceLifecycleError>>;

    fn authorization_snapshot<'a>(
        &'a self,
        principal: Principal,
        workspace_id: WorkspaceId,
        now_millis: i64,
    ) -> PortFuture<'a, Result<Option<WorkspaceAuthorizationSnapshotV1>, WorkspaceLifecycleError>>;

    fn changes_after<'a>(
        &'a self,
        workspace_id: WorkspaceId,
        authorization_version: u64,
        limit: u32,
    ) -> PortFuture<'a, Result<Vec<WorkspaceCloudChange>, WorkspaceLifecycleError>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_device::DeviceSigningKey;

    #[test]
    fn activation_transcript_is_exact_and_signature_bound() {
        let key = DeviceSigningKey::from_secret_bytes([7; 32]);
        let challenge = WorkspaceActivationChallenge {
            version: 1,
            challenge_id: Uuid::from_u128(1),
            operation_id: Uuid::from_u128(2),
            workspace_id: WorkspaceId::from_uuid(Uuid::from_u128(3)),
            region: Region::Us,
            owner_profile_id: ProfileId::from_uuid(Uuid::from_u128(4)),
            owner_account_id: "55555555-5555-4555-8555-555555555555".parse().unwrap(),
            device_id: DeviceId::from_uuid(Uuid::from_u128(6)),
            session_id: "77777777-7777-4777-8777-777777777777".parse().unwrap(),
            issuer: "https://us.api.koru.do".into(),
            registered_device_signing_public_key: key.public_key(),
            expected_authorization_version: 0,
            expected_profile_lifecycle_revision: 0,
            nonce: [8; 32],
            issued_at_millis: 10,
            expires_at_millis: 300_010,
            consumed_at_millis: None,
        };
        let signature = key.sign_message(&challenge.signing_bytes());
        assert!(challenge.verifies(signature));
        let mut changed = challenge;
        changed.operation_id = Uuid::from_u128(9);
        assert!(!changed.verifies(signature));
    }

    #[test]
    fn lifecycle_errors_have_stable_codes() {
        assert_eq!(WorkspaceLifecycleError::Deleting.code(), "cloud_deleting");
        assert_eq!(
            WorkspaceLifecycleError::OperationReplay.code(),
            "workspace_operation_replay"
        );
    }
}
