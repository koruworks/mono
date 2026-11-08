// SPDX-License-Identifier: MPL-2.0

//! The claims a Cloud sync session is admitted under, whichever kind of
//! credential carried them.
//!
//! A worker admits two credentials through one handshake: a device's
//! [`SyncCredentialClaimsV1`] and an app connection's
//! [`AppConnectionSyncCredentialClaimsV1`]. They share every dataset, epoch
//! and endpoint field and differ only in who holds them, so admission reads
//! one view, [`SessionClaims`], whose [`SessionPrincipal`] says which. The
//! two credentials still verify only as themselves: [`verify_session_credential`]
//! tries the device's header type and, only when that header is not the
//! device's, the connection's. Neither is a fallback for the other's failure.

use koru_account_core::{
    AppConnectionId, KoruIdentityId,
    workspace_authorization::{AppConnectionAccess, Region},
};
use koru_device::{ReplicationEndpointId, cloud::CloudId};

use super::{
    SyncCredentialClaimsV1,
    app_connection::{
        AppConnectionStage, AppConnectionSyncCredentialClaimsV1, verify_app_connection_credential,
    },
    verify_credential,
};
use crate::{
    TokenCapabilityVerificationError as Error, TokenCapabilityVerifier, VerifiedTokenCapability,
};

/// Who holds the credential a session was admitted under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionPrincipal {
    /// A registered device. Its key signs the handshake transcript.
    Device { device_id: CloudId },
    /// An app connection's replica. It has no device and no device key; the
    /// credential's endpoint key signs the transcript, and `access` and
    /// `stage` bound which units it may sync and in which direction.
    AppConnection {
        connection_id: AppConnectionId,
        access: AppConnectionAccess,
        stage: AppConnectionStage,
    },
}

/// The verified claims both credential kinds share, under the device
/// credential's field names, plus who holds them. Built only from claims this
/// crate verified; there is no deserializer.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionClaims {
    pub principal: SessionPrincipal,
    pub region: Region,
    pub sub: KoruIdentityId,
    pub profile_id: CloudId,
    pub client_endpoint_id: ReplicationEndpointId,
    pub workspace_id: CloudId,
    pub workspace_incarnation_id: CloudId,
    pub workspace_replica_id: CloudId,
    pub profile_lineage_id: CloudId,
    pub profile_replica_id: CloudId,
    pub authorization_version: i64,
    pub membership_epoch: i64,
    pub cloud_access_epoch: i64,
    pub iat: i64,
    pub exp: i64,
}

impl From<SyncCredentialClaimsV1> for SessionClaims {
    fn from(claims: SyncCredentialClaimsV1) -> Self {
        Self {
            principal: SessionPrincipal::Device {
                device_id: claims.device_id,
            },
            region: claims.region,
            sub: claims.sub,
            profile_id: claims.profile_id,
            client_endpoint_id: claims.client_endpoint_id,
            workspace_id: claims.workspace_id,
            workspace_incarnation_id: claims.workspace_incarnation_id,
            workspace_replica_id: claims.workspace_replica_id,
            profile_lineage_id: claims.profile_lineage_id,
            profile_replica_id: claims.profile_replica_id,
            authorization_version: claims.authorization_version,
            membership_epoch: claims.membership_epoch,
            cloud_access_epoch: claims.cloud_access_epoch,
            iat: claims.iat,
            exp: claims.exp,
        }
    }
}

impl From<AppConnectionSyncCredentialClaimsV1> for SessionClaims {
    fn from(claims: AppConnectionSyncCredentialClaimsV1) -> Self {
        Self {
            principal: SessionPrincipal::AppConnection {
                connection_id: claims.connection_id,
                access: claims.access,
                stage: claims.stage,
            },
            region: claims.region,
            sub: claims.sub,
            profile_id: claims.profile_id,
            client_endpoint_id: claims.client_endpoint_id,
            workspace_id: claims.workspace_id,
            workspace_incarnation_id: claims.workspace_incarnation_id,
            workspace_replica_id: claims.workspace_replica_id,
            profile_lineage_id: claims.profile_lineage_id,
            profile_replica_id: claims.profile_replica_id,
            authorization_version: claims.authorization_version,
            membership_epoch: claims.membership_epoch,
            cloud_access_epoch: claims.cloud_access_epoch,
            iat: claims.iat,
            exp: claims.exp,
        }
    }
}

/// Verify a sync credential of either kind. The device's header type is tried
/// first; only a credential whose header is not the device's (`wrong_purpose`
/// before its claims are read) is tried as an app connection's. Every other
/// refusal of the device verifier is final, so a damaged device credential is
/// never re-read as something else.
pub fn verify_session_credential(
    verifier: &TokenCapabilityVerifier,
    token: &[u8],
    now_millis: i64,
) -> Result<VerifiedTokenCapability<SessionClaims>, Error> {
    match verify_credential(verifier, token, now_millis) {
        Ok(device) => Ok(device.map(SessionClaims::from)),
        Err(Error::WrongPurpose) => verify_app_connection_credential(verifier, token, now_millis)
            .map(|connection| connection.map(SessionClaims::from)),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
