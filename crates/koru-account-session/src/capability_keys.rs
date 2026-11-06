// SPDX-License-Identifier: MPL-2.0

//! The regional token/capability signing purpose, separate from login and
//! enrollment. Purpose-specific signing adds no hosted issuance route.
//! Relay issuance is outside this rung.
use crate::{Ed25519KeyRing, TokenCapabilityVerifier};
use ed25519_dalek::{SigningKey, VerifyingKey};
use koru_account_core::{CryptoError, TokenSigner, workspace_authorization::Region};
use std::collections::BTreeMap;

#[derive(Clone)]
pub struct TokenCapabilityKeyRing {
    region: Region,
    ring: Ed25519KeyRing,
}

impl TokenCapabilityKeyRing {
    /// Trusted composition supplies the regional issuer, never a request field.
    /// This signs evidence, not an authorization decision. Hosted issuance must
    /// complete all current session/device/dataset/route checks first.
    pub fn sign_sync_credential(
        &self,
        issuer: &str,
        claims: &crate::cloud::SyncCredentialClaimsV1,
    ) -> Result<crate::cloud::issuance::SignedSyncCredential, CryptoError> {
        crate::cloud::issuance::validate_signing_claims(self.region, issuer, claims)?;
        let token = self.ring.sign_compact("koru-sync-credential+jwt", claims)?;
        if token.compact.expose_secret().len() > crate::cloud::MAX_COMPACT_BYTES {
            return Err(CryptoError::InvalidInput);
        }
        Ok(crate::cloud::issuance::SignedSyncCredential::new(
            token, claims.exp,
        ))
    }

    /// Sign an app connection's sync credential. The same rule applies: the
    /// connection, its access, the entitlement and the Workspace's current
    /// authority must already have been read and checked by the caller.
    pub fn sign_app_connection_sync_credential(
        &self,
        issuer: &str,
        claims: &crate::cloud::app_connection::AppConnectionSyncCredentialClaimsV1,
    ) -> Result<crate::cloud::issuance::SignedSyncCredential, CryptoError> {
        crate::cloud::app_connection::validate_signing_claims(self.region, issuer, claims)?;
        let token = self.ring.sign_compact(
            crate::cloud::app_connection::APP_CONNECTION_SYNC_TYPE,
            claims,
        )?;
        if token.compact.expose_secret().len() > crate::cloud::MAX_COMPACT_BYTES {
            return Err(CryptoError::InvalidInput);
        }
        Ok(crate::cloud::issuance::SignedSyncCredential::new(
            token, claims.exp,
        ))
    }

    /// Mint one object capability for the blob byte routes. The same rule as
    /// above applies and matters more here: this signs evidence a *separate*
    /// service trusts without any database of its own, so every lifecycle,
    /// ownership, entitlement, region, epoch and incarnation check must already
    /// have passed before this is called.
    pub fn sign_blob_capability(
        &self,
        issuer: &str,
        claims: &crate::cloud::blob::BlobCapabilityClaimsV1,
    ) -> Result<crate::cloud::blob::SignedBlobCapability, CryptoError> {
        crate::cloud::blob::validate_signing_claims(self.region, issuer, claims)?;
        let token = self
            .ring
            .sign_compact(crate::cloud::blob::BLOB_CAPABILITY_TYPE, claims)?;
        if token.compact.expose_secret().len() > crate::cloud::MAX_COMPACT_BYTES {
            return Err(CryptoError::InvalidInput);
        }
        Ok(crate::cloud::blob::SignedBlobCapability::new(
            token, claims.exp,
        ))
    }

    pub fn new(
        region: Region,
        active: String,
        keys: impl IntoIterator<Item = (String, SigningKey)>,
    ) -> Result<Self, CryptoError> {
        Ok(Self {
            region,
            ring: Ed25519KeyRing::new(active, keys)?,
        })
    }

    pub fn region(&self) -> Region {
        self.region
    }

    pub fn verification_keys(&self) -> BTreeMap<String, VerifyingKey> {
        self.ring.verification_keys()
    }

    /// Drops all signing material when constructing the shared verification seam.
    pub fn verifier(&self, issuer: String) -> Result<TokenCapabilityVerifier, CryptoError> {
        TokenCapabilityVerifier::new(self.region, issuer, self.verification_keys())
    }

    pub fn public_jwks(&self) -> Result<String, CryptoError> {
        self.ring.public_jwks()
    }
}
