// SPDX-License-Identifier: MPL-2.0

//! What both host adapters build after a dial succeeds: the Cloud principal
//! the gateway installs, formed from the live permit,
//! the credential the handshake just verified, and the acceptance's facts.
//! Nothing here dials, verifies or stores; it cross-checks and shapes.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use koru_account_session::cloud::SyncCredentialClaimsV1;
use koru_account_wire::cloud_sync::SyncCredential;
use koru_device::ReplicationEndpointId;
use koru_device::cloud::{
    CLOUD_PRINCIPAL_VERSION, CloudId, CloudPrincipalV1, CloudReplicaPrincipalV1,
};
use koru_sync_control::cloud::{CloudPermitScopeV2, CloudStage};
use uuid::Uuid;

use super::{CloudPermitV2, Failure, Stage};

/// The claims of a compact credential the handshake has already verified.
/// This reads the same bytes the verifier accepted; it is never called on a
/// credential that has not been through [`koru_network_iroh::cloud::authenticate`]
/// first, so the signature is not checked a second time here.
pub fn credential_claims(credential: &SyncCredential) -> Result<SyncCredentialClaimsV1, Failure> {
    let mut parts = credential.compact().split('.');
    let _header = parts.next().ok_or(Failure::Unavailable)?;
    let payload = parts.next().ok_or(Failure::Unavailable)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| Failure::Unavailable)?;
    serde_json::from_slice(&bytes).map_err(|_| Failure::Unavailable)
}

/// What the worker's acceptance established for this dataset session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accepted {
    pub session_generation: u64,
    pub expires_at_seconds: i64,
}

fn cloud(value: Uuid) -> CloudId {
    CloudId::from_bytes(*value.as_bytes())
}

fn positive(value: i64) -> Result<u64, Failure> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(Failure::DatasetMismatch)
}

/// Form the principal for one dataset session. Every identity it carries is
/// cross-checked between the permit and the verified claims; a disagreement
/// is `DatasetMismatch`, never reconciled by taking one field from each.
///
/// Since root-catalog Plan 3.2 the permit is one scope's. For a Workspace
/// permit the claims must name the permit's Workspace and incarnation, and
/// the principal's `profile_id` is the claims' — a placement hint the v1
/// credential shape still carries, never an admission input. For a
/// Profile-units permit the claims must name the permit's Profile, and the
/// authorizing Workspace fields are the claims', because the permit names
/// none: the v1 principal shape requires them and the credential is the
/// one place they are stated.
pub fn build_principal(
    permit: &CloudPermitV2,
    claims: &SyncCredentialClaimsV1,
    local_endpoint: ReplicationEndpointId,
    worker_endpoint: ReplicationEndpointId,
    control_session_id: Uuid,
    accepted: Accepted,
) -> Result<CloudPrincipalV1, Failure> {
    if cloud(claims.sub.as_uuid()) != permit.account_id
        || claims.client_endpoint_id != local_endpoint
    {
        return Err(Failure::DatasetMismatch);
    }
    let dataset = permit.dataset().ok_or(Failure::DatasetMismatch)?;
    let (profile_id, authority_workspace_id, authority_workspace_incarnation_id) =
        match &permit.scope {
            CloudPermitScopeV2::Workspace { descriptor } => {
                if claims.workspace_id != descriptor.workspace_id
                    || claims.workspace_incarnation_id != descriptor.workspace_incarnation_id
                {
                    return Err(Failure::DatasetMismatch);
                }
                (
                    claims.profile_id,
                    descriptor.workspace_id,
                    descriptor.workspace_incarnation_id,
                )
            }
            CloudPermitScopeV2::ProfileUnits { dataset, .. } => {
                let CloudReplicaPrincipalV1::Profile { profile_id, .. } = dataset else {
                    return Err(Failure::DatasetMismatch);
                };
                if claims.profile_id != *profile_id {
                    return Err(Failure::DatasetMismatch);
                }
                (
                    *profile_id,
                    claims.workspace_id,
                    claims.workspace_incarnation_id,
                )
            }
            CloudPermitScopeV2::UserCatalog | CloudPermitScopeV2::UserPersonal { .. } => {
                return Err(Failure::DatasetMismatch);
            }
        };
    let principal = CloudPrincipalV1 {
        version: CLOUD_PRINCIPAL_VERSION,
        permit_id: permit.permit_id,
        account_id: permit.account_id,
        profile_id,
        dataset,
        client_endpoint_id: local_endpoint,
        worker_endpoint_id: worker_endpoint,
        credential_id: claims.jti,
        authorization_version: positive(claims.authorization_version)?,
        membership_epoch: positive(claims.membership_epoch)?,
        cloud_access_epoch: positive(claims.cloud_access_epoch)?,
        authority_workspace_id,
        authority_workspace_incarnation_id,
        control_session_id,
        account_generation: permit.account_generation,
        session_generation: accepted.session_generation,
        expires_at_seconds: accepted.expires_at_seconds,
    };
    principal.validate().map_err(|_| Failure::DatasetMismatch)?;
    Ok(principal)
}

impl From<CloudStage> for Stage {
    fn from(stage: CloudStage) -> Self {
        match stage {
            CloudStage::AwaitingAuthority => Self::AwaitingAuthority,
            CloudStage::Connecting => Self::Connecting,
            CloudStage::Reconciling => Self::Reconciling,
            CloudStage::WaitingForConsent => Self::WaitingForConsent,
            CloudStage::Offline => Self::Offline,
            CloudStage::Unavailable => Self::Unavailable,
            CloudStage::CheckingCoverage => Self::CheckingCoverage,
            CloudStage::Settled => Self::Settled,
        }
    }
}

/// The stable codes a host may carry back into a [`super::SessionReport`],
/// which holds a static string. A code the driver could report but this list
/// does not know collapses to `cloud_session_failed` rather than being
/// dropped, and the exact code is still in the gateway's status.
pub fn static_code(code: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        super::CREDENTIAL_EXPIRED,
        super::SESSION_EXPIRED,
        "sync_transfer_invalid",
        "sync_peer_closed",
        "sync_transfer_timeout",
        "sync_policy_changed",
        "sync_update_too_large",
        "sync_unavailable",
        // An app connection's creation of a unit Cloud already holds (the
        // admission contract's amendment, DS-D44): the hosted MCP server's
        // creation refusal, never a device's.
        "sync_unit_exists",
        "replica_fenced",
        "manifest_mismatch",
        "manifest_unavailable",
        "profile_absent",
        "workspace_provisioning_failed",
        "store_unavailable",
        "store_worker_unavailable",
        "cloud_permit_descriptor_required",
        "cloud_permit_descriptor_mismatch",
        "cloud_permit_profile_mismatch",
        "cloud_permit_profile_units",
        "cloud_permit_scope",
        "cloud_connection_closed",
        "cloud_session_timeout",
        // Root-catalog Plan 3.2: a Workspace whose root placement this
        // device cannot list it under yet, and a subject whose device-local
        // mode closes the Cloud path. Both retried, neither a broken
        // bootstrap.
        super::PLACEMENT_PENDING,
        "mode_refuses_transport",
        // A pass that finished while one of its units did not. Distinct from
        // every code above it on purpose: those end a session, this one
        // describes a session that ran to the end with a unit left behind.
        "cloud_unit_unavailable",
    ];
    KNOWN
        .iter()
        .copied()
        .find(|known| *known == code)
        .unwrap_or("cloud_session_failed")
}

#[cfg(test)]
mod tests;
