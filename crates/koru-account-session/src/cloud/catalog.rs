// SPDX-License-Identifier: MPL-2.0

//! The account catalog's scope credential, `CatalogScopeCredentialClaimsV2`
//! (root-catalog `docs/cloud-platform/contracts/root-catalog-cloud-v2.md`
//! §5; the private service's checklist item 3).
//!
//! A v2 sync token names one scope. Two of the four — `workspace` and
//! `profile_units` — are content scopes with a v1 dataset principal, and
//! their credential is the v1 device credential exactly as the workers admit
//! it today. The other two — `user_catalog` and `user_personal` — have no
//! dataset: their session is the account catalog's, keyed by the catalog
//! scope the binding minted. This is that credential: the same issuer,
//! audience, region and device facts as the v1 claims, with the catalog
//! scope and the one admission scope in place of a dataset, under its own
//! header type and purpose so that no worker that verifies v1 credentials
//! ever admits one by accident.
//!
//! **No carrier session kind admits this credential yet.** Until the worker
//! side grows a principal arm for the catalog scope, root and personal units
//! travel over the account API's HTTP routes, and this credential's live
//! content is its expiry and its authority generation — what a
//! `CloudPermitV2` for the scope is fenced by. This module holds the claims,
//! the signing validation and the verification, so the day the session kind
//! exists it verifies what the service has been issuing all along.

use koru_account_core::{CryptoError, KoruIdentityId, workspace_authorization::Region};
use koru_device::{ReplicationEndpointId, cloud::CloudId};
use serde::{Deserialize, Serialize};

use super::{bounded, endpoint, epoch};
use crate::{
    TokenCapabilityClaims, TokenCapabilityFields, TokenCapabilityRequirements,
    TokenCapabilityVerificationError as Error, TokenCapabilityVerifier, VerifiedTokenCapability,
};

/// The compact JWS header type. Distinct from the device credential's
/// `koru-sync-credential+jwt` and the app connection's, so a credential of
/// one kind presented as another fails at the header before its claims are
/// read.
pub const CATALOG_SCOPE_SYNC_TYPE: &str = "koru-sync-catalog-scope+jwt";

/// The credential purpose, compared exactly.
pub const CATALOG_SCOPE_SYNC_PURPOSE: &str = "koru:sync:catalog-scope:v2";

/// The same sync audience as a device credential.
pub const CATALOG_SCOPE_SYNC_AUDIENCE: &str = "koru-sync";

/// Fifteen minutes, the shared verifier's ceiling, never more.
pub const CATALOG_SCOPE_SYNC_MAX_SECONDS: i64 = 900;

/// Which of the two catalog scopes the credential is for (contract §5): the
/// account's root units, or the account's personal unit for one Workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CatalogScopeKind {
    UserCatalog,
    UserPersonal { workspace_id: CloudId },
}

/// The strict shape, checked in both directions: unknown, duplicate, missing
/// and null fields are refused, and every id spelling is validated in
/// deserialization.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogScopeCredentialClaimsV2 {
    pub ver: u32,
    pub purpose: String,
    pub iss: String,
    pub aud: String,
    pub region: Region,
    /// The account whose catalog this is.
    pub sub: KoruIdentityId,
    /// The scope the account's binding minted; never a request field.
    pub catalog_scope_id: CloudId,
    pub device_id: CloudId,
    #[serde(deserialize_with = "endpoint")]
    pub client_endpoint_id: ReplicationEndpointId,
    pub scope: CatalogScopeKind,
    /// The scope's authority generation, which the permit built from the
    /// token carries and a stale permit is fenced by.
    #[serde(deserialize_with = "epoch")]
    pub authority_generation: i64,
    pub iat: i64,
    pub exp: i64,
    pub jti: CloudId,
}

impl std::fmt::Debug for CatalogScopeCredentialClaimsV2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogScopeCredentialClaimsV2")
            .field("scope", &self.scope)
            .field("exp", &self.exp)
            .finish_non_exhaustive()
    }
}

impl TokenCapabilityClaims for CatalogScopeCredentialClaimsV2 {
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

/// Verify a catalog scope credential. Cryptographic evidence only: whatever
/// admits a catalog session still reads the account's binding and the scope's
/// current generation before it admits anything.
pub fn verify_catalog_scope_credential(
    verifier: &TokenCapabilityVerifier,
    token: &[u8],
    now_millis: i64,
) -> Result<VerifiedTokenCapability<CatalogScopeCredentialClaimsV2>, Error> {
    bounded(token)?;
    let requirements = TokenCapabilityRequirements::new(
        CATALOG_SCOPE_SYNC_TYPE.into(),
        2,
        CATALOG_SCOPE_SYNC_PURPOSE.into(),
        CATALOG_SCOPE_SYNC_AUDIENCE.into(),
        CATALOG_SCOPE_SYNC_MAX_SECONDS * 1000,
    )
    .expect("compiled Cloud policy");
    let verified: VerifiedTokenCapability<CatalogScopeCredentialClaimsV2> =
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
    claims: &CatalogScopeCredentialClaimsV2,
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
        || claims.ver != 2
        || claims.purpose != CATALOG_SCOPE_SYNC_PURPOSE
        || claims.aud != CATALOG_SCOPE_SYNC_AUDIENCE
        || claims.iat < 0
        || claims.authority_generation < 1
        || !(1..=CATALOG_SCOPE_SYNC_MAX_SECONDS).contains(&lifetime)
        || claims.iat.checked_mul(1_000).is_none()
        || claims.exp.checked_mul(1_000).is_none()
    {
        return Err(CryptoError::InvalidInput);
    }
    let bytes = serde_json::to_vec(claims).map_err(|_| CryptoError::InvalidInput)?;
    serde_json::from_slice::<CatalogScopeCredentialClaimsV2>(&bytes)
        .map_err(|_| CryptoError::InvalidInput)?;
    Ok(())
}

#[cfg(test)]
mod tests;
