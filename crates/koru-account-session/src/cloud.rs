// SPDX-License-Identifier: MPL-2.0

//! Complete Cloud claim shapes over the existing purpose-isolated verifier.
//! Bounded signing primitives have no authority lookup, cache or network effect.

pub mod app_connection;
pub mod blob;
pub mod issuance;
pub mod session;

use crate::{
    TokenCapabilityClaims, TokenCapabilityFields, TokenCapabilityRequirements,
    TokenCapabilityVerificationError as Error, TokenCapabilityVerifier, VerifiedTokenCapability,
};
use koru_account_core::{KoruIdentityId, workspace_authorization::Region};
use koru_device::{ReplicationEndpointId, cloud::CloudId};
use serde::{Deserialize, Deserializer, Serialize};

pub const MAX_COMPACT_BYTES: usize = 10_240;

fn endpoint<'de, D: Deserializer<'de>>(d: D) -> Result<ReplicationEndpointId, D::Error> {
    let value = ReplicationEndpointId::deserialize(d)?;
    value
        .validate_cloud_key()
        .map_err(serde::de::Error::custom)?;
    Ok(value)
}

fn epoch<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let value = i64::deserialize(d)?;
    if value < 0 {
        return Err(serde::de::Error::custom("negative authority epoch"));
    }
    Ok(value)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncCredentialClaimsV1 {
    pub ver: u32,
    pub purpose: String,
    pub iss: String,
    pub aud: String,
    pub region: Region,
    pub sub: KoruIdentityId,
    pub profile_id: CloudId,
    pub device_id: CloudId,
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
    pub iat: i64,
    pub exp: i64,
    pub jti: CloudId,
}

macro_rules! capability {
    ($ty:ty) => {
        impl TokenCapabilityClaims for $ty {
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
    };
}
capability!(SyncCredentialClaimsV1);

fn bounded(token: &[u8]) -> Result<(), Error> {
    if token.is_empty() || token.len() > MAX_COMPACT_BYTES || !token.is_ascii() {
        return Err(Error::Malformed);
    }
    Ok(())
}

pub fn verify_credential(
    verifier: &TokenCapabilityVerifier,
    token: &[u8],
    now_millis: i64,
) -> Result<VerifiedTokenCapability<SyncCredentialClaimsV1>, Error> {
    bounded(token)?;
    let requirements = TokenCapabilityRequirements::new(
        "koru-sync-credential+jwt".into(),
        1,
        "koru:sync:device:v1".into(),
        "koru-sync".into(),
        900_000,
    )
    .expect("compiled Cloud policy");
    let verified: VerifiedTokenCapability<SyncCredentialClaimsV1> =
        verifier.verify(token, &requirements, now_millis)?;
    if verified.claims().region != Region::Us {
        return Err(Error::WrongRegion);
    }
    Ok(verified)
}

#[cfg(test)]
mod tests;
