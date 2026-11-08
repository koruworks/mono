// SPDX-License-Identifier: MPL-2.0

//! The app connection's sync credential, `AppConnectionSyncCredentialClaimsV1`.
//!
//! An app connection (an AI assistant, for one) reaches Cloud through an
//! in-memory replica the hosted service keeps for it. That replica dials the
//! Cloud workers like a device does, but it is not a device: it has no
//! `DeviceId`, no device key and no trust. So its credential is the device
//! credential's claims with `connection_id` in place of `device_id`, plus the
//! connection's `access` and the replica's `stage`, under its own header type
//! and purpose. The two kinds never verify as each other.
//!
//! This module holds the credential only: claims, signing validation and
//! verification. Which units a stage admits, refusing uploads under `read`
//! and the live connection read are the worker's, at admission.

use koru_account_core::{
    AppConnectionId, CryptoError, KoruIdentityId,
    workspace_authorization::{AppConnectionAccess, Region},
};
use koru_device::{ReplicationEndpointId, cloud::CloudId};
use serde::{Deserialize, Serialize};

use super::{bounded, endpoint, epoch};
use crate::{
    TokenCapabilityClaims, TokenCapabilityFields, TokenCapabilityRequirements,
    TokenCapabilityVerificationError as Error, TokenCapabilityVerifier, VerifiedTokenCapability,
};

/// The compact JWS header type. Distinct from the device credential's
/// `koru-sync-credential+jwt`, so a credential of either kind presented as the
/// other fails at the header (`wrong_purpose`) before its claims are read.
pub const APP_CONNECTION_SYNC_TYPE: &str = "koru-sync-app-connection+jwt";

/// The credential purpose, compared exactly.
pub const APP_CONNECTION_SYNC_PURPOSE: &str = "koru:sync:app-connection:v1";

/// The same sync audience as a device credential: the same workers admit it.
pub const APP_CONNECTION_SYNC_AUDIENCE: &str = "koru-sync";

/// Ten minutes, as a device credential, unless the entitlement ends sooner.
pub const APP_CONNECTION_SYNC_DEFAULT_SECONDS: i64 = 600;

/// Fifteen minutes, the shared verifier's ceiling, never more.
pub const APP_CONNECTION_SYNC_MAX_SECONDS: i64 = 900;

/// Which units the replica holding this credential may sync.
///
/// - `manifest`: the Workspace manifest and its Profile's Identity unit.
/// - `full`: those and every document unit of the Workspace.
///
/// Neither stage ever admits a Private or Personal unit, and Identity is
/// download-only at both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppConnectionStage {
    Manifest,
    Full,
}

/// The strict shape, checked in both directions: unknown, duplicate, missing
/// and null fields are refused, and every id spelling is validated in
/// deserialization. Field order is the device credential's, with
/// `connection_id` where `device_id` stands and `access` and `stage` after the
/// epochs.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConnectionSyncCredentialClaimsV1 {
    pub ver: u32,
    pub purpose: String,
    pub iss: String,
    pub aud: String,
    pub region: Region,
    /// The account the connection belongs to.
    pub sub: KoruIdentityId,
    pub profile_id: CloudId,
    pub connection_id: AppConnectionId,
    /// The replica's own endpoint key, generated when its process starts and
    /// never stored. It is also the key that signs the handshake transcript,
    /// because a connection has no device key.
    #[serde(deserialize_with = "endpoint")]
    pub client_endpoint_id: ReplicationEndpointId,
    pub workspace_id: CloudId,
    pub workspace_incarnation_id: CloudId,
    pub workspace_replica_id: CloudId,
    pub profile_lineage_id: CloudId,
    pub profile_replica_id: CloudId,
    #[serde(deserialize_with = "epoch")]
    pub authorization_version: i64,
    #[serde(deserialize_with = "epoch")]
    pub membership_epoch: i64,
    #[serde(deserialize_with = "epoch")]
    pub cloud_access_epoch: i64,
    pub access: AppConnectionAccess,
    pub stage: AppConnectionStage,
    pub iat: i64,
    pub exp: i64,
    pub jti: CloudId,
}

impl TokenCapabilityClaims for AppConnectionSyncCredentialClaimsV1 {
    fn verification_fields(&self) -> TokenCapabilityFields<'_> {
        TokenCapabilityFields {
            version: self.ver,
            purpose: &self.purpose,
            issuer: &self.iss,
            audience: &self.aud,
            region: self.region,
            issued_at_seconds: self.iat,
            expires_at_seconds: self.exp,
        }
    }
}

/// Verify an app connection's sync credential. Cryptographic evidence only:
/// the worker still reads the live connection, its access and its resolved
/// Workspaces before it admits a session.
pub fn verify_app_connection_credential(
    verifier: &TokenCapabilityVerifier,
    token: &[u8],
    now_millis: i64,
) -> Result<VerifiedTokenCapability<AppConnectionSyncCredentialClaimsV1>, Error> {
    bounded(token)?;
    let requirements = TokenCapabilityRequirements::new(
        APP_CONNECTION_SYNC_TYPE.into(),
        1,
        APP_CONNECTION_SYNC_PURPOSE.into(),
        APP_CONNECTION_SYNC_AUDIENCE.into(),
        APP_CONNECTION_SYNC_MAX_SECONDS * 1000,
    )
    .expect("compiled Cloud policy");
    let verified: VerifiedTokenCapability<AppConnectionSyncCredentialClaimsV1> =
        verifier.verify(token, &requirements, now_millis)?;
    if verified.claims().region != Region::Us {
        return Err(Error::WrongRegion);
    }
    Ok(verified)
}

/// The same strictness applied before signing: claims can be constructed
/// directly in Rust, so the decoder runs over them too.
pub(crate) fn validate_signing_claims(
    region: Region,
    issuer: &str,
    claims: &AppConnectionSyncCredentialClaimsV1,
) -> Result<(), CryptoError> {
    let lifetime = claims
        .exp
        .checked_sub(claims.iat)
        .ok_or(CryptoError::InvalidInput)?;
    if region != Region::Us
        || claims.region != region
        || issuer.is_empty()
        || issuer.len() > 512
        || issuer.chars().any(|c| c.is_whitespace() || c.is_control())
        || claims.iss != issuer
        || claims.ver != 1
        || claims.purpose != APP_CONNECTION_SYNC_PURPOSE
        || claims.aud != APP_CONNECTION_SYNC_AUDIENCE
        || claims.iat < 0
        || !(1..=APP_CONNECTION_SYNC_MAX_SECONDS).contains(&lifetime)
        || claims.iat.checked_mul(1_000).is_none()
        || claims.exp.checked_mul(1_000).is_none()
    {
        return Err(CryptoError::InvalidInput);
    }
    let bytes = serde_json::to_vec(claims).map_err(|_| CryptoError::InvalidInput)?;
    serde_json::from_slice::<AppConnectionSyncCredentialClaimsV1>(&bytes)
        .map_err(|_| CryptoError::InvalidInput)?;
    Ok(())
}

#[cfg(test)]
mod tests;
