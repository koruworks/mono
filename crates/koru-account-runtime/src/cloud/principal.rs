// SPDX-License-Identifier: MPL-2.0

//! What both host adapters build after a dial succeeds: the Cloud principal
//! the gateway installs, formed from the live permit,
//! the credential the handshake just verified, and the acceptance's facts.
//! Nothing here dials, verifies or stores; it cross-checks and shapes.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use koru_account_session::cloud::SyncCredentialClaimsV1;
use koru_account_wire::cloud_sync::SyncCredential;
use koru_device::ReplicationEndpointId;
use koru_device::cloud::{CLOUD_PRINCIPAL_VERSION, CloudId, CloudPrincipalV1};
use koru_sync_control::cloud::{
    CloudPermitV1 as ControlPermit, CloudStage, CloudWorkspaceDescriptorV1 as ControlDescriptor,
};
use uuid::Uuid;

use super::{CloudPermitV1, Failure, Stage};

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
pub fn build_principal(
    permit: &CloudPermitV1,
    claims: &SyncCredentialClaimsV1,
    local_endpoint: ReplicationEndpointId,
    worker_endpoint: ReplicationEndpointId,
    control_session_id: Uuid,
    accepted: Accepted,
) -> Result<CloudPrincipalV1, Failure> {
    if claims.sub != permit.account_id
        || claims.profile_id != cloud(permit.profile_id.as_uuid())
        || claims.workspace_id != permit.authority_workspace_id
        || claims.workspace_incarnation_id != permit.authority_workspace_incarnation_id
        || claims.client_endpoint_id != local_endpoint
    {
        return Err(Failure::DatasetMismatch);
    }
    let principal = CloudPrincipalV1 {
        version: CLOUD_PRINCIPAL_VERSION,
        permit_id: permit.permit_id,
        account_id: cloud(permit.account_id.as_uuid()),
        profile_id: cloud(permit.profile_id.as_uuid()),
        dataset: permit.dataset,
        client_endpoint_id: local_endpoint,
        worker_endpoint_id: worker_endpoint,
        credential_id: claims.jti,
        authorization_version: positive(claims.authorization_version)?,
        membership_epoch: positive(claims.membership_epoch)?,
        cloud_access_epoch: positive(claims.cloud_access_epoch)?,
        authority_workspace_id: permit.authority_workspace_id,
        authority_workspace_incarnation_id: permit.authority_workspace_incarnation_id,
        control_session_id,
        account_generation: permit.account_generation,
        session_generation: accepted.session_generation,
        expires_at_seconds: accepted.expires_at_seconds,
    };
    principal.validate().map_err(|_| Failure::DatasetMismatch)?;
    Ok(principal)
}

/// The control-protocol form of a runtime permit: the same fields, with the
/// account-core identities carried as canonical UUIDs. The desktop sends it
/// over control 7; the Android host hands it to the same `ControlHost` in
/// process.
pub fn control_permit(permit: &CloudPermitV1) -> ControlPermit {
    ControlPermit {
        version: permit.version,
        permit_id: permit.permit_id,
        dataset: permit.dataset,
        account_id: cloud(permit.account_id.as_uuid()),
        profile_id: cloud(permit.profile_id.as_uuid()),
        authority_workspace_id: permit.authority_workspace_id,
        authority_workspace_incarnation_id: permit.authority_workspace_incarnation_id,
        account_generation: permit.account_generation,
        expires_at_seconds: permit.expires_at_seconds,
        allowed_profile_units: permit.allowed_profile_units.clone(),
        workspace_descriptor: permit.workspace_descriptor.as_ref().map(|descriptor| {
            ControlDescriptor {
                version: 1,
                profile_id: cloud(descriptor.profile_id.as_uuid()),
                workspace_id: cloud(descriptor.workspace_id.as_uuid()),
                workspace_incarnation_id: cloud(descriptor.workspace_incarnation_id),
                logical_replica_id: cloud(descriptor.logical_replica_id),
                lineage_id: cloud(descriptor.lineage_id),
                manifest_document_id: cloud(descriptor.manifest_document_id),
                authorization_version: descriptor.authorization_version,
                membership_epoch: descriptor.membership_epoch,
                cloud_access_epoch: descriptor.cloud_access_epoch,
            }
        }),
    }
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
        "cloud_connection_closed",
        "cloud_session_timeout",
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
