// SPDX-License-Identifier: MPL-2.0

//! Shared verification for the token/capability signing purpose. The concrete
//! credential's strict decoder and resource bindings belong to its consumer.
//! This module has no issuance, authority lookup, cache, or persisted format.

use std::collections::BTreeMap;

use ed25519_dalek::VerifyingKey;
use koru_account_core::{CryptoError, workspace_authorization::Region};
use serde::de::DeserializeOwned;

use crate::jwt::{CompactVerificationError, verify_compact};

const MAX_LIFETIME_MILLIS: i64 = 15 * 60 * 1_000;
const MAX_VERIFICATION_KEYS: usize = 16;

/// Trusted consumer configuration. No value is selected by the signed token.
pub struct TokenCapabilityRequirements {
    header_type: String,
    version: u32,
    purpose: String,
    audience: String,
    max_lifetime_millis: i64,
}

impl TokenCapabilityRequirements {
    pub fn new(
        header_type: String,
        version: u32,
        purpose: String,
        audience: String,
        max_lifetime_millis: i64,
    ) -> Result<Self, CryptoError> {
        if !valid_label(&header_type, 128)
            || version == 0
            || !valid_label(&purpose, 256)
            || !valid_label(&audience, 256)
            || max_lifetime_millis <= 0
            || max_lifetime_millis > MAX_LIFETIME_MILLIS
        {
            return Err(CryptoError::InvalidInput);
        }
        Ok(Self {
            header_type,
            version,
            purpose,
            audience,
            max_lifetime_millis,
        })
    }
}

/// Borrowed view of the consumer's decoded claims, not a signed wire shape.
/// JWT timestamps are seconds; conversion and arithmetic are checked here.
pub struct TokenCapabilityFields<'a> {
    pub version: u32,
    pub purpose: &'a str,
    pub issuer: &'a str,
    pub audience: &'a str,
    pub region: Region,
    pub issued_at_seconds: i64,
    pub expires_at_seconds: i64,
}

/// Implement only for a complete strict signed shape: reject unknown, duplicate,
/// missing and null fields, and validate every resource/id representation in
/// deserialization. Return the fields from that same decoded value. Plan 13
/// owns the sync shape; implementing this trait does not confer authority.
pub trait TokenCapabilityClaims: DeserializeOwned {
    fn verification_fields(&self) -> TokenCapabilityFields<'_>;
}

/// A public-key-only verifier. Composition supplies only the token/capability
/// purpose's keys, never a union with account or enrollment verification keys.
pub struct TokenCapabilityVerifier {
    region: Region,
    issuer: String,
    keys: BTreeMap<String, VerifyingKey>,
}

impl TokenCapabilityVerifier {
    pub fn new(
        region: Region,
        issuer: String,
        keys: impl IntoIterator<Item = (String, VerifyingKey)>,
    ) -> Result<Self, CryptoError> {
        if !valid_label(&issuer, 512) {
            return Err(CryptoError::InvalidInput);
        }
        let mut collected = BTreeMap::new();
        for (id, key) in keys {
            if collected.len() >= MAX_VERIFICATION_KEYS
                || id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
                || collected.insert(id, key).is_some()
            {
                return Err(CryptoError::InvalidInput);
            }
        }
        if collected.is_empty() {
            return Err(CryptoError::KeyUnavailable);
        }
        Ok(Self {
            region,
            issuer,
            keys: collected,
        })
    }

    /// Verifies signature before decoding claims. The strict consumer shape is
    /// retained whole: no workspace, replica, endpoint or epoch is projected out.
    /// Current membership, revocation and lifecycle must still be checked at use.
    pub fn verify<C: TokenCapabilityClaims>(
        &self,
        token: &[u8],
        requirements: &TokenCapabilityRequirements,
        now_millis: i64,
    ) -> Result<VerifiedTokenCapability<C>, TokenCapabilityVerificationError> {
        use TokenCapabilityVerificationError as Error;
        let compact = verify_compact(token, &self.keys, &requirements.header_type, 10 * 1024)?;
        let claims: C = serde_json::from_slice(&compact.payload).map_err(|_| Error::Malformed)?;
        let fields = claims.verification_fields();
        if fields.version != requirements.version {
            return Err(Error::UnsupportedVersion);
        }
        if fields.purpose != requirements.purpose {
            return Err(Error::WrongPurpose);
        }
        if fields.audience != requirements.audience {
            return Err(Error::WrongAudience);
        }
        if fields.region != self.region {
            return Err(Error::WrongRegion);
        }
        if fields.issuer != self.issuer {
            return Err(Error::WrongIssuer);
        }
        let issued_at_millis = fields
            .issued_at_seconds
            .checked_mul(1_000)
            .ok_or(Error::InvalidClaims)?;
        let expires_at_millis = fields
            .expires_at_seconds
            .checked_mul(1_000)
            .ok_or(Error::InvalidClaims)?;
        let lifetime = expires_at_millis
            .checked_sub(issued_at_millis)
            .ok_or(Error::InvalidClaims)?;
        if issued_at_millis < 0 || lifetime <= 0 || lifetime > requirements.max_lifetime_millis {
            return Err(Error::InvalidClaims);
        }
        verify_time(issued_at_millis, expires_at_millis, now_millis)?;
        Ok(VerifiedTokenCapability {
            claims,
            issued_at_millis,
            expires_at_millis,
        })
    }
}

/// Cryptographic evidence only, deliberately not a workspace admission permit.
/// There is no deserializer or public constructor for a verified result.
pub struct VerifiedTokenCapability<C> {
    claims: C,
    issued_at_millis: i64,
    expires_at_millis: i64,
}

impl<C> VerifiedTokenCapability<C> {
    pub fn claims(&self) -> &C {
        &self.claims
    }

    /// A session must check this at use as well as reconciling current authority.
    /// No expiry grace or expired-evidence receipt mode exists for capabilities.
    pub fn verify_time_at(&self, now_millis: i64) -> Result<(), TokenCapabilityVerificationError> {
        verify_time(self.issued_at_millis, self.expires_at_millis, now_millis)
    }

    /// The same verified evidence under a projection of its claims, keeping
    /// its verified times. Crate-private: only a projection this crate defines
    /// over claims it verified itself may carry the evidence on.
    pub(crate) fn map<D>(self, project: impl FnOnce(C) -> D) -> VerifiedTokenCapability<D> {
        VerifiedTokenCapability {
            claims: project(self.claims),
            issued_at_millis: self.issued_at_millis,
            expires_at_millis: self.expires_at_millis,
        }
    }
}

impl<C> std::fmt::Debug for VerifiedTokenCapability<C> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("VerifiedTokenCapability(<verified-evidence>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenCapabilityVerificationError {
    Malformed,
    UnknownKey,
    Signature,
    InvalidClaims,
    UnsupportedVersion,
    WrongPurpose,
    WrongAudience,
    WrongRegion,
    WrongIssuer,
    CredentialNotYetValid,
    CredentialExpired,
}

impl TokenCapabilityVerificationError {
    /// Content-free observation shared by adapters; do not emit credential prose.
    pub fn code(self) -> &'static str {
        match self {
            Self::Malformed => "credential_malformed",
            Self::UnknownKey => "credential_unknown_key",
            Self::Signature => "credential_signature_invalid",
            Self::InvalidClaims => "credential_claims_invalid",
            Self::UnsupportedVersion => "credential_version_unsupported",
            Self::WrongPurpose => "wrong_purpose",
            Self::WrongAudience => "wrong_audience",
            Self::WrongRegion => "wrong_region",
            Self::WrongIssuer => "wrong_issuer",
            Self::CredentialNotYetValid => "credential_not_yet_valid",
            Self::CredentialExpired => "credential_expired",
        }
    }
}

impl From<CompactVerificationError> for TokenCapabilityVerificationError {
    fn from(error: CompactVerificationError) -> Self {
        match error {
            CompactVerificationError::Malformed => Self::Malformed,
            CompactVerificationError::InvalidHeader => Self::InvalidClaims,
            CompactVerificationError::WrongType => Self::WrongPurpose,
            CompactVerificationError::UnknownKey => Self::UnknownKey,
            CompactVerificationError::Signature => Self::Signature,
        }
    }
}

fn verify_time(
    issued_at_millis: i64,
    expires_at_millis: i64,
    now_millis: i64,
) -> Result<(), TokenCapabilityVerificationError> {
    if now_millis < issued_at_millis {
        Err(TokenCapabilityVerificationError::CredentialNotYetValid)
    } else if now_millis >= expires_at_millis {
        Err(TokenCapabilityVerificationError::CredentialExpired)
    } else {
        Ok(())
    }
}

fn valid_label(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod tests;
