// SPDX-License-Identifier: MPL-2.0

//! The two tokens Koru's OAuth authorization server signs for app
//! connections (Cloud MCP TECHNICAL.final §6.1, §7.3).
//!
//! - **The MCP access token**, `typ` `at+jwt`, purpose `koru:mcp:access:v1`,
//!   on the token/capability ring. A separate service (the MCP host) verifies
//!   it with public keys alone, exactly as the Cloud workers verify a sync
//!   credential, so it lives on the ring whose purpose is that. It is never an
//!   account access token: those are `typ` `JWT` on the access ring, and each
//!   verifier refuses the other at the header.
//! - **The pending authorization request**, purpose `koru:oauth:request:v1`,
//!   on the access ring, for ten minutes. It carries the validated
//!   `/oauth/authorize` request through the website's consent page, so the
//!   pending request needs no table. Only the API that signed it reads it.
//!
//! Signing validates the complete claims first; verification is the shared
//! purpose-isolated verifier plus each shape's strict decoder. Neither is an
//! authorization decision: the connection, its access and the entitlement are
//! read live by the caller.

use koru_account_core::{
    AppConnectionId, CryptoError, KoruIdentityId, SignedToken,
    workspace_authorization::{AppConnectionAccess, Region},
};
use koru_device::cloud::CloudId;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    Ed25519KeyRing, TokenCapabilityClaims, TokenCapabilityFields, TokenCapabilityRequirements,
    TokenCapabilityVerificationError as Error, TokenCapabilityVerifier, VerifiedTokenCapability,
};

/// The access token's JWS header type (RFC 9068).
pub const MCP_ACCESS_TYPE: &str = "at+jwt";
/// The access token's purpose, compared exactly.
pub const MCP_ACCESS_PURPOSE: &str = "koru:mcp:access:v1";
/// Fifteen minutes, like an account access token.
pub const MCP_ACCESS_SECONDS: i64 = 900;

/// The pending request's JWS header type.
pub const OAUTH_REQUEST_TYPE: &str = "koru-oauth-request+jwt";
/// The pending request's purpose, compared exactly.
pub const OAUTH_REQUEST_PURPOSE: &str = "koru:oauth:request:v1";
/// The pending request is addressed to this API's own consent routes.
pub const OAUTH_REQUEST_AUDIENCE: &str = "koru-oauth-consent";
/// Ten minutes: the consent cookie's lifetime, sign-in included.
pub const OAUTH_REQUEST_SECONDS: i64 = 600;

/// The two OAuth scope values rung 1 knows.
pub const SCOPE_DOCUMENTS_READ: &str = "documents:read";
pub const SCOPE_DOCUMENTS_WRITE: &str = "documents:write";

/// Bounds of the request token's free-text fields.
pub const MAX_CLIENT_ID_BYTES: usize = 2048;
pub const MAX_REDIRECT_URI_BYTES: usize = 2048;
pub const MAX_STATE_BYTES: usize = 512;
pub const MAX_RESOURCE_BYTES: usize = 2048;

/// A token's scope: exactly `documents:read`, or `documents:read
/// documents:write` in that order. Read & edit never omits read, and no other
/// spelling decodes. It is an [`AppConnectionAccess`] on the wire as a string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct McpScope(pub AppConnectionAccess);

impl McpScope {
    pub const READ: Self = Self(AppConnectionAccess::Read);
    pub const READ_WRITE: Self = Self(AppConnectionAccess::ReadWrite);

    pub const fn as_str(self) -> &'static str {
        match self.0 {
            AppConnectionAccess::Read => "documents:read",
            AppConnectionAccess::ReadWrite => "documents:read documents:write",
        }
    }

    /// The scope values, one per element, as the database ceiling stores them.
    pub const fn values(self) -> &'static [&'static str] {
        match self.0 {
            AppConnectionAccess::Read => &[SCOPE_DOCUMENTS_READ],
            AppConnectionAccess::ReadWrite => &[SCOPE_DOCUMENTS_READ, SCOPE_DOCUMENTS_WRITE],
        }
    }

    /// The exact signed spelling only.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "documents:read" => Some(Self::READ),
            "documents:read documents:write" => Some(Self::READ_WRITE),
            _ => None,
        }
    }

    /// A request's `scope` parameter (RFC 6749 §3.3): space-separated, any
    /// order, repeats tolerated. `documents:write` implies `documents:read`,
    /// because an edit tool reads first. An absent or empty parameter asks for
    /// everything the server offers. An unknown value is `None`.
    pub fn parse_request(value: Option<&str>) -> Option<Self> {
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            return Some(Self::READ_WRITE);
        };
        let mut write = false;
        for token in value.split(' ').filter(|token| !token.is_empty()) {
            match token {
                SCOPE_DOCUMENTS_READ => {}
                SCOPE_DOCUMENTS_WRITE => write = true,
                _ => return None,
            }
        }
        Some(if write { Self::READ_WRITE } else { Self::READ })
    }

    /// The narrower of the two: what an issuance may carry.
    pub const fn intersect(self, other: Self) -> Self {
        match (self.0, other.0) {
            (AppConnectionAccess::ReadWrite, AppConnectionAccess::ReadWrite) => Self::READ_WRITE,
            _ => Self::READ,
        }
    }

    pub const fn access(self) -> AppConnectionAccess {
        self.0
    }
}

impl Serialize for McpScope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for McpScope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).ok_or_else(|| serde::de::Error::custom("unknown MCP scope"))
    }
}

/// The MCP access token's claims, strict in both directions.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpAccessTokenClaimsV1 {
    pub ver: u32,
    pub purpose: String,
    pub iss: String,
    /// The protected resource, `https://mcp.koru.do/mcp` (RFC 8707).
    pub aud: String,
    pub region: Region,
    /// The account the connection belongs to.
    pub sub: KoruIdentityId,
    /// The OAuth client the token was issued to (RFC 9068 §2.2).
    pub client_id: String,
    pub connection: AppConnectionId,
    pub scope: McpScope,
    pub iat: i64,
    pub exp: i64,
    pub jti: CloudId,
}

impl std::fmt::Debug for McpAccessTokenClaimsV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpAccessTokenClaimsV1")
            .field("connection", &self.connection)
            .field("scope", &self.scope)
            .field("exp", &self.exp)
            .finish_non_exhaustive()
    }
}

/// The pending authorization request, as `/oauth/authorize` validated it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthRequestClaimsV1 {
    pub ver: u32,
    pub purpose: String,
    pub iss: String,
    pub aud: String,
    pub region: Region,
    pub client_id: String,
    pub redirect_uri: String,
    /// The client's opaque `state`, returned on the redirect unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// S256 only: the unpadded base64url of a SHA-256.
    pub code_challenge: String,
    pub resource: String,
    /// What the client asked for; the person may narrow it at consent.
    pub scope: McpScope,
    /// The website locale the consent page was opened in.
    pub locale: String,
    pub iat: i64,
    pub exp: i64,
    pub jti: CloudId,
}

impl std::fmt::Debug for OAuthRequestClaimsV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthRequestClaimsV1")
            .field("scope", &self.scope)
            .field("exp", &self.exp)
            .finish_non_exhaustive()
    }
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
capability!(McpAccessTokenClaimsV1);
capability!(OAuthRequestClaimsV1);

fn bounded(token: &[u8]) -> Result<(), Error> {
    if token.is_empty() || token.len() > crate::cloud::MAX_COMPACT_BYTES || !token.is_ascii() {
        return Err(Error::Malformed);
    }
    Ok(())
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn valid_issuer(issuer: &str) -> bool {
    !issuer.is_empty()
        && issuer.len() <= 512
        && !issuer.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn valid_lifetime(iat: i64, exp: i64, maximum: i64) -> bool {
    iat >= 0
        && exp
            .checked_sub(iat)
            .is_some_and(|lifetime| (1..=maximum).contains(&lifetime))
        && iat.checked_mul(1_000).is_some()
        && exp.checked_mul(1_000).is_some()
}

/// The same strictness the verifier applies, before signing.
pub(crate) fn validate_access_claims(
    region: Region,
    issuer: &str,
    claims: &McpAccessTokenClaimsV1,
) -> Result<(), CryptoError> {
    if region != Region::Us
        || claims.region != region
        || !valid_issuer(issuer)
        || claims.iss != issuer
        || claims.ver != 1
        || claims.purpose != MCP_ACCESS_PURPOSE
        || !valid_issuer(&claims.aud)
        || !valid_text(&claims.client_id, MAX_CLIENT_ID_BYTES)
        || !valid_lifetime(claims.iat, claims.exp, MCP_ACCESS_SECONDS)
    {
        return Err(CryptoError::InvalidInput);
    }
    let bytes = serde_json::to_vec(claims).map_err(|_| CryptoError::InvalidInput)?;
    serde_json::from_slice::<McpAccessTokenClaimsV1>(&bytes)
        .map_err(|_| CryptoError::InvalidInput)?;
    Ok(())
}

fn validate_request_claims(issuer: &str, claims: &OAuthRequestClaimsV1) -> Result<(), CryptoError> {
    if claims.region != Region::Us
        || !valid_issuer(issuer)
        || claims.iss != issuer
        || claims.ver != 1
        || claims.purpose != OAUTH_REQUEST_PURPOSE
        || claims.aud != OAUTH_REQUEST_AUDIENCE
        || !valid_text(&claims.client_id, MAX_CLIENT_ID_BYTES)
        || !valid_text(&claims.redirect_uri, MAX_REDIRECT_URI_BYTES)
        || !valid_text(&claims.resource, MAX_RESOURCE_BYTES)
        || claims
            .state
            .as_deref()
            .is_some_and(|state| !valid_text(state, MAX_STATE_BYTES))
        || claims.code_challenge.len() != 43
        || !claims
            .code_challenge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        || !valid_text(&claims.locale, 16)
        || !valid_lifetime(claims.iat, claims.exp, OAUTH_REQUEST_SECONDS)
    {
        return Err(CryptoError::InvalidInput);
    }
    Ok(())
}

impl Ed25519KeyRing {
    /// Sign a validated pending authorization request on this (the access)
    /// ring. Nothing is authorized by it: approval needs a live web session,
    /// and the code it leads to is single-use and bound to the PKCE challenge.
    pub fn sign_oauth_request(
        &self,
        issuer: &str,
        claims: &OAuthRequestClaimsV1,
    ) -> Result<SignedToken, CryptoError> {
        validate_request_claims(issuer, claims)?;
        let token = self.sign_compact(OAUTH_REQUEST_TYPE, claims)?;
        if token.compact.expose_secret().len() > crate::cloud::MAX_COMPACT_BYTES {
            return Err(CryptoError::InvalidInput);
        }
        Ok(token)
    }
}

/// Verify an MCP access token for `resource`. Cryptographic evidence only:
/// the connection, its access and its Workspaces are read live at admission.
pub fn verify_mcp_access_token(
    verifier: &TokenCapabilityVerifier,
    token: &[u8],
    resource: &str,
    now_millis: i64,
) -> Result<VerifiedTokenCapability<McpAccessTokenClaimsV1>, Error> {
    bounded(token)?;
    let requirements = TokenCapabilityRequirements::new(
        MCP_ACCESS_TYPE.into(),
        1,
        MCP_ACCESS_PURPOSE.into(),
        resource.into(),
        MCP_ACCESS_SECONDS * 1_000,
    )
    .map_err(|_| Error::InvalidClaims)?;
    let verified: VerifiedTokenCapability<McpAccessTokenClaimsV1> =
        verifier.verify(token, &requirements, now_millis)?;
    if verified.claims().region != Region::Us {
        return Err(Error::WrongRegion);
    }
    Ok(verified)
}

/// Verify a pending authorization request this API signed. The verifier must
/// hold the access ring's public keys and nothing else.
pub fn verify_oauth_request(
    verifier: &TokenCapabilityVerifier,
    token: &[u8],
    now_millis: i64,
) -> Result<VerifiedTokenCapability<OAuthRequestClaimsV1>, Error> {
    bounded(token)?;
    let requirements = TokenCapabilityRequirements::new(
        OAUTH_REQUEST_TYPE.into(),
        1,
        OAUTH_REQUEST_PURPOSE.into(),
        OAUTH_REQUEST_AUDIENCE.into(),
        OAUTH_REQUEST_SECONDS * 1_000,
    )
    .expect("compiled OAuth request policy");
    let verified: VerifiedTokenCapability<OAuthRequestClaimsV1> =
        verifier.verify(token, &requirements, now_millis)?;
    if validate_request_claims(&verified.claims().iss, verified.claims()).is_err() {
        return Err(Error::InvalidClaims);
    }
    Ok(verified)
}

#[cfg(test)]
mod tests;
