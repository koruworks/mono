// SPDX-License-Identifier: MPL-2.0

//! The `cloud-blob-object-v1` object capability: claims, signing validation and
//! verification, beside the sync credential it deliberately reuses.
//!
//! This module holds the *credential* only. The constants, stable codes, range
//! admission and wire shapes of the contract live in `koru-cloud-blob`, which
//! depends on this crate; the one number both need — the capability lifetime —
//! is defined here and asserted equal there at compile time, because two
//! numbers in two files is how the recovery budget and the checkpoint trigger
//! came to contradict each other (D243).
//!
//! A capability is evidence, never authority. The object worker verifies one
//! and moves bytes; durability is the API's, after its own authoritative read.

use koru_account_core::{CryptoError, workspace_authorization::Region};
use koru_device::cloud::CloudId;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    TokenCapabilityClaims, TokenCapabilityFields, TokenCapabilityRequirements,
    TokenCapabilityVerificationError as Error, TokenCapabilityVerifier, VerifiedTokenCapability,
};

/// The compact JWS header type, frozen by the contract's §6.
pub const BLOB_CAPABILITY_TYPE: &str = "koru-blob-cap+jws";

/// The content address of an empty file. Zero length is valid only here.
pub const EMPTY_BLOB_HASH: &str =
    "b3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

/// `CAPABILITY_LIFETIME` — five minutes (D250). It is also the post-revocation
/// byte window: an admitted read capability is the authorization for reads for
/// as long as it lives, because the object worker holds no account database.
pub const BLOB_CAPABILITY_DEFAULT_SECONDS: i64 = 300;

/// The two purposes, compared exactly. A read capability used to write — or the
/// reverse — is a malformed credential, not a permission failure: at rung 7 one
/// of these tokens is handed to a third party (cases C05/C06).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobPurpose {
    Read,
    Write,
}

impl BlobPurpose {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "blob_read",
            Self::Write => "blob_write",
        }
    }
}

fn epoch<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let value = i64::deserialize(d)?;
    if value < 0 {
        return Err(serde::de::Error::custom("negative Cloud-access epoch"));
    }
    Ok(value)
}

/// `b3:<hex>` — the canonical spelling, never the `b3-<hex>` filename. The two
/// spellings never cross (D248), so this decoder refuses the filename form.
fn blob_hash<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = String::deserialize(d)?;
    if !valid_blob_hash(&value) {
        return Err(serde::de::Error::custom("not a canonical b3:<hex> address"));
    }
    Ok(value)
}

/// The strict shape checked in both directions: unknown, duplicate, missing and
/// null fields are refused, and every id spelling is validated in deserialization.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobCapabilityClaimsV1 {
    /// Contract version; `1`.
    pub v: u32,
    /// `blob_read` or `blob_write`.
    pub pur: String,
    pub iss: String,
    pub aud: String,
    pub rgn: Region,
    /// The account the capability was minted for.
    pub acc: CloudId,
    pub wsp: CloudId,
    /// Dataset incarnation at issuance.
    pub inc: CloudId,
    /// Cloud-access epoch at issuance.
    #[serde(deserialize_with = "epoch")]
    pub eph: i64,
    /// The upload id (`blob_write`) or the object locator (`blob_read`).
    pub sub: CloudId,
    /// The immutable object locator this capability's bytes belong to.
    ///
    /// **Not in the contract's §6 table, and it has to be.** Before D252 split
    /// the services, the process that published an object was the one that had
    /// minted the locator; the object worker holds no account database and can
    /// learn it from nowhere else. Letting the client name the publication
    /// target instead would let a client choose another Workspace's object
    /// path, so it is a signed claim. On a `blob_read` capability it equals
    /// `sub`, which is checked in both directions. Recorded in QUESTIONS.md.
    pub loc: CloudId,
    #[serde(deserialize_with = "blob_hash")]
    pub bh: String,
    /// Exact plaintext length.
    pub len: u64,
    /// Not-before, whole seconds.
    pub nbf: i64,
    /// Expiry, whole seconds.
    pub exp: i64,
    pub jti: CloudId,
}

impl TokenCapabilityClaims for BlobCapabilityClaimsV1 {
    fn verification_fields(&self) -> TokenCapabilityFields<'_> {
        TokenCapabilityFields {
            version: self.v,
            purpose: &self.pur,
            issuer: &self.iss,
            audience: &self.aud,
            region: self.rgn,
            // The contract spells the lower bound `nbf`; the shared verifier
            // calls the same instant `issued_at` and gives it no tolerance.
            issued_at_seconds: self.nbf,
            expires_at_seconds: self.exp,
        }
    }
}

/// `b3:<64 lowercase hex>`, the address a document carries. Kept here rather
/// than reached for from `koru-store-core` so the hosted credential crate does
/// not depend on the local Store model.
pub fn valid_blob_hash(value: &str) -> bool {
    let Some((algorithm, digest)) = value.split_once(':') else {
        return false;
    };
    algorithm == "b3"
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Verify one object capability for exactly the purpose the route implies.
///
/// Region, audience, issuer, version, purpose, signature and window are all
/// compared here; the resource bindings — which upload, which workspace, which
/// incarnation, which epoch — belong to the caller, because only it knows the
/// world the request arrived in.
pub fn verify_blob_capability(
    verifier: &TokenCapabilityVerifier,
    token: &[u8],
    purpose: BlobPurpose,
    audience: &str,
    region: Region,
    now_millis: i64,
) -> Result<VerifiedTokenCapability<BlobCapabilityClaimsV1>, Error> {
    if token.is_empty() || token.len() > super::MAX_COMPACT_BYTES || !token.is_ascii() {
        return Err(Error::Malformed);
    }
    let requirements = TokenCapabilityRequirements::new(
        BLOB_CAPABILITY_TYPE.into(),
        1,
        purpose.as_str().into(),
        audience.into(),
        BLOB_CAPABILITY_DEFAULT_SECONDS * 1_000,
    )
    .map_err(|_| Error::InvalidClaims)?;
    verifier
        .verify(token, &requirements, now_millis)
        .map_err(|error| region_first(token, region, error))
}

/// **Region outranks every other refusal**, which the shared verifier's own
/// order does not say.
///
/// `TokenCapabilityVerifier` compares audience before region, and its exact
/// behaviour is a frozen vector of `cloud-sync-admission-v1`, so it does not
/// move. But a capability minted for one region names that region's object
/// host, so *both* differ at once — and the contract's C03 and C04 say the
/// answer is `wrong_region`, in both directions. A caller told
/// `capability_invalid` would go looking for a broken signature instead of a
/// misrouted request.
///
/// So the refusal is reclassified, and only the refusal: this never turns a
/// refusal into an admission, and it reads the region out of a payload whose
/// signature may not have verified — which is safe, because the caller sent
/// that payload and both outcomes are a denial.
fn region_first(token: &[u8], region: Region, error: Error) -> Error {
    if matches!(error, Error::WrongRegion) {
        return error;
    }
    let Some(payload) = std::str::from_utf8(token)
        .ok()
        .and_then(|text| text.split('.').nth(1))
        .and_then(|segment| {
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, segment).ok()
        })
    else {
        return error;
    };
    #[derive(serde::Deserialize)]
    struct JustTheRegion {
        rgn: Region,
    }
    match serde_json::from_slice::<JustTheRegion>(&payload) {
        Ok(claimed) if claimed.rgn != region => Error::WrongRegion,
        _ => error,
    }
}

/// The same strictness applied before signing: claims can be constructed
/// directly in Rust, so the decoder runs over them too rather than trusting
/// that a caller built a shape the verifier would accept.
pub(crate) fn validate_signing_claims(
    region: Region,
    issuer: &str,
    claims: &BlobCapabilityClaimsV1,
) -> Result<(), CryptoError> {
    let lifetime = claims
        .exp
        .checked_sub(claims.nbf)
        .ok_or(CryptoError::InvalidInput)?;
    if claims.rgn != region
        || issuer.is_empty()
        || issuer.len() > 512
        || issuer.chars().any(|c| c.is_whitespace() || c.is_control())
        || claims.iss != issuer
        || claims.v != 1
        || (claims.pur != BlobPurpose::Read.as_str() && claims.pur != BlobPurpose::Write.as_str())
        || claims.aud.is_empty()
        || claims.aud.len() > 256
        || claims
            .aud
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
        || claims.nbf < 0
        || !(1..=BLOB_CAPABILITY_DEFAULT_SECONDS).contains(&lifetime)
        || (claims.len == 0 && claims.bh != EMPTY_BLOB_HASH)
        || (claims.pur == BlobPurpose::Read.as_str() && claims.loc != claims.sub)
        || claims.nbf.checked_mul(1_000).is_none()
        || claims.exp.checked_mul(1_000).is_none()
        || !valid_blob_hash(&claims.bh)
    {
        return Err(CryptoError::InvalidInput);
    }
    let bytes = serde_json::to_vec(claims).map_err(|_| CryptoError::InvalidInput)?;
    serde_json::from_slice::<BlobCapabilityClaimsV1>(&bytes)
        .map_err(|_| CryptoError::InvalidInput)?;
    Ok(())
}

/// Backdate a fresh capability's `nbf` by this much, for the reason
/// [`super::issuance::SYNC_CREDENTIAL_ISSUED_BACKDATE_SECONDS`] records: the
/// shared verifier allows **no** tolerance, so a worker whose clock is one
/// millisecond behind the API would refuse every capability it is handed. Two
/// containers on one host still disagree. The expiry moves with it, so this
/// spends 30 seconds of a five-minute capability and grants nothing.
pub const BLOB_CAPABILITY_NOT_BEFORE_BACKDATE_SECONDS: i64 = 30;

/// The `nbf`, in whole seconds, to sign for a capability minted now. Never
/// negative: a zero-ish clock must not produce an unsignable capability.
pub fn blob_capability_not_before(now_millis: i64) -> i64 {
    (now_millis / 1000)
        .saturating_sub(BLOB_CAPABILITY_NOT_BEFORE_BACKDATE_SECONDS)
        .max(0)
}

/// The matching `exp`. Whole-second arithmetic, separate from the verifier's
/// millisecond representation.
pub fn blob_capability_expiry(not_before_seconds: i64) -> Option<i64> {
    if not_before_seconds < 0 {
        return None;
    }
    not_before_seconds.checked_add(BLOB_CAPABILITY_DEFAULT_SECONDS)
}

/// A signed object capability. `Debug` never exposes the bearer material.
pub struct SignedBlobCapability {
    token: koru_account_core::SignedToken,
    expires_at_seconds: i64,
}

impl SignedBlobCapability {
    pub(crate) fn new(token: koru_account_core::SignedToken, expires_at_seconds: i64) -> Self {
        Self {
            token,
            expires_at_seconds,
        }
    }
    pub fn compact(&self) -> &[u8] {
        self.token.compact.expose_secret()
    }
    pub fn key_id(&self) -> &str {
        &self.token.key_id
    }
    pub fn expires_at_seconds(&self) -> i64 {
        self.expires_at_seconds
    }
}

impl std::fmt::Debug for SignedBlobCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SignedBlobCapability(<redacted>)")
    }
}

#[cfg(test)]
mod tests;
