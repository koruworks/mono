// SPDX-License-Identifier: MPL-2.0

//! The vendor port: authorize, refresh, read one item, revoke.
//!
//! One adapter per qualified vendor implements [`ConnectionVendor`]. The
//! service above never names a vendor and never sees a vendor's own text: an
//! adapter maps every failure to the closed [`VendorError`] and discards the
//! response body. An adapter sends a token only to the HTTPS origins it
//! declares and follows no redirect.
//!
//! Every type here that can hold a secret, the content that was read, or
//! private text (an account label, a title, a vendor's item reference, the
//! consent URL) has a `Debug` that prints none of it.

use std::fmt;
use std::time::Instant;

use koru_ai_core::connections::ProviderToken;
use koru_oauth_credentials::SecretString;

/// The most text one read returns, in bytes of UTF-8 (256 KiB, provisional).
pub const READ_LIMIT_BYTES: usize = 262_144;

/// Where the person is sent to consent. Not a secret, and not logged either:
/// it is handed to the shell only to be opened.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthorizationRequest {
    consent_url: String,
}
impl AuthorizationRequest {
    pub fn new(consent_url: impl Into<String>) -> Self {
        Self {
            consent_url: consent_url.into(),
        }
    }
    pub fn consent_url(&self) -> &str {
        &self.consent_url
    }
}
impl fmt::Debug for AuthorizationRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthorizationRequest([REDACTED])")
    }
}

/// What came back on the redirect. `code` is absent when the person did not
/// consent. Its query string is never logged.
pub struct AuthorizationCallback {
    pub state: String,
    pub code: Option<SecretString>,
}
impl fmt::Debug for AuthorizationCallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthorizationCallback([REDACTED])")
    }
}

/// A short-lived access token. It lives in memory only.
#[derive(Clone)]
pub struct AccessToken {
    pub secret: SecretString,
    pub expires_at: Instant,
}
impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken([REDACTED])")
    }
}

/// The vendor's own identifier of one item. Private metadata.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct VendorItemRef(String);
impl VendorItemRef {
    /// `None` unless it fits the stored column (1 to 1024 bytes).
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (1..=1024).contains(&value.len()).then_some(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for VendorItemRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VendorItemRef([REDACTED])")
    }
}

/// What an item was when the person picked it: a digest of the vendor's item
/// identifier and of the item's owning account ([`crate::item_identity_digest`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ItemIdentityDigest(pub [u8; 32]);
impl fmt::Debug for ItemIdentityDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ItemIdentityDigest(..)")
    }
}

/// One item the person picked during the consent trip.
#[derive(Clone)]
pub struct PickedItem {
    pub reference: VendorItemRef,
    pub identity: ItemIdentityDigest,
    pub title: String,
}
impl fmt::Debug for PickedItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PickedItem([REDACTED])")
    }
}

/// The account a consent trip ended in.
pub struct AuthorizedAccount {
    /// The vendor's stable account identifier. Only its digest is stored.
    pub subject: String,
    /// What the person recognizes, for example an email address.
    pub label: String,
    pub scopes: Vec<String>,
    pub refresh: SecretString,
    pub access: AccessToken,
    pub items: Vec<PickedItem>,
}
impl fmt::Debug for AuthorizedAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthorizedAccount([REDACTED])")
    }
}

/// A refreshed access token, and the new refresh token when the vendor
/// rotates it.
pub struct RefreshedAccess {
    pub access: AccessToken,
    pub rotated_refresh: Option<SecretString>,
}
impl fmt::Debug for RefreshedAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RefreshedAccess([REDACTED])")
    }
}

/// The text of one item. `truncated` and `total_bytes` say when `text` is
/// not all of it; it is never silently shortened.
pub struct ItemContent {
    pub text: String,
    pub truncated: bool,
    pub total_bytes: u64,
}
impl fmt::Debug for ItemContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ItemContent")
            .field("bytes", &self.text.len())
            .field("truncated", &self.truncated)
            .field("total_bytes", &self.total_bytes)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevokeOutcome {
    Revoked,
    /// The vendor has no revoke call.
    Unsupported,
}

/// Every way a vendor call can fail. Closed, and it carries no vendor text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VendorError {
    /// This build has no registration with the vendor.
    NotConfigured,
    ConsentDenied,
    /// The stored credential is refused.
    InvalidGrant,
    Unauthorized,
    Forbidden,
    NotFound,
    /// The item is another item, or belongs to another account, than the one
    /// the person picked. No content was read.
    IdentityChanged,
    RateLimited,
    Rejected,
    /// Nothing was sent, or the transport failed before the request left.
    Unavailable,
    /// The request left and no trustworthy answer came back.
    Unconfirmed,
}
impl fmt::Display for VendorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotConfigured => "not_configured",
            Self::ConsentDenied => "consent_denied",
            Self::InvalidGrant => "invalid_grant",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::IdentityChanged => "identity_changed",
            Self::RateLimited => "rate_limited",
            Self::Rejected => "rejected",
            Self::Unavailable => "unavailable",
            Self::Unconfirmed => "unconfirmed",
        })
    }
}
impl std::error::Error for VendorError {}

/// One vendor, behind the four operations the contract names.
pub trait ConnectionVendor: Send + Sync {
    /// The token ai.db stores for this vendor.
    fn provider(&self) -> ProviderToken;
    /// An HTTPS origin: where the credential slot is scoped.
    fn service_origin(&self) -> &str;

    /// Builds the consent URL. No network. Authorize, step one.
    fn authorization_request(
        &self,
        redirect_uri: &str,
        state: &str,
        pkce_challenge: &str,
    ) -> Result<AuthorizationRequest, VendorError>;

    /// Exchanges the callback for tokens and reads the account identity and
    /// the picked items. Authorize, step two.
    fn complete_authorization(
        &self,
        callback: &AuthorizationCallback,
        pkce_verifier: &SecretString,
        redirect_uri: &str,
        deadline: Instant,
    ) -> Result<AuthorizedAccount, VendorError>;

    /// Refresh. A rotated refresh token is returned when the vendor rotates.
    fn refresh(
        &self,
        refresh: &SecretString,
        deadline: Instant,
    ) -> Result<RefreshedAccess, VendorError>;

    /// Pure parsing of a link or id the person supplied. No network.
    fn item_reference(&self, text: &str) -> Option<VendorItemRef>;

    /// Read item. First reads the item's current identity (its id and its
    /// owning account) and compares its digest with `expected_identity`; on a
    /// mismatch returns `IdentityChanged` and reads no content (D17). Then
    /// returns at most `limit_bytes` of UTF-8 text.
    fn read_item(
        &self,
        access: &SecretString,
        item: &VendorItemRef,
        expected_identity: &ItemIdentityDigest,
        limit_bytes: usize,
        deadline: Instant,
    ) -> Result<ItemContent, VendorError>;

    /// Revoke at the vendor.
    fn revoke(
        &self,
        refresh: &SecretString,
        deadline: Instant,
    ) -> Result<RevokeOutcome, VendorError>;
}
