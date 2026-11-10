// SPDX-License-Identifier: MPL-2.0

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use koru_account_core::workspace_lifecycle::{WorkspaceCloudState, WorkspaceLifecycleStatus};
use koru_account_core::{
    AccountSessionId, AuthFlowId, AuthIdentityId, AuthProvider, DeviceDescriptorV1, DeviceId,
    DevicePlatform, DeviceProfileState, KoruIdentityId, ProfileId, ProfileLifecycle,
    ProfileLifecycleMutationId, ProfileLifecycleRevision, WorkspaceId,
};
use koru_device::{DevicePublicKey, ReplicationEndpointId, SignatureBytes};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::Version1;

macro_rules! secret_string {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }
    };
}

secret_string!(
    /// Provider authorization code.
    AuthorizationCode
);
secret_string!(
    /// Raw Google ID token delivered by the native account sheet.
    GoogleIdToken
);
secret_string!(
    /// OAuth correlation state.
    OAuthState
);
secret_string!(
    /// Client PKCE verifier.
    PkceVerifier
);
secret_string!(
    /// One-use account grant.
    OneUseGrant
);
secret_string!(
    /// Short-lived Koru API bearer.
    AccessToken
);
secret_string!(
    /// Rotating opaque Koru refresh token.
    RefreshToken
);
/// Canonical base64url-no-pad encoding of one 256-bit exact refresh retry key.
#[derive(Clone, PartialEq, Eq)]
pub struct RefreshIdempotencyKey(String);

impl RefreshIdempotencyKey {
    pub fn from_bytes(value: [u8; 32]) -> Self {
        Self(URL_SAFE_NO_PAD.encode(value))
    }

    pub fn parse(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        if value.len() != 43 {
            return None;
        }
        let decoded = URL_SAFE_NO_PAD.decode(value.as_bytes()).ok()?;
        if decoded.len() != 32 || URL_SAFE_NO_PAD.encode(&decoded) != value {
            return None;
        }
        Some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn decode(&self) -> [u8; 32] {
        let decoded = URL_SAFE_NO_PAD
            .decode(self.0.as_bytes())
            .expect("RefreshIdempotencyKey is validated on construction");
        decoded
            .try_into()
            .expect("RefreshIdempotencyKey always contains exactly 32 bytes")
    }
}

impl fmt::Debug for RefreshIdempotencyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RefreshIdempotencyKey(<redacted>)")
    }
}

impl Serialize for RefreshIdempotencyKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RefreshIdempotencyKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).ok_or_else(|| de::Error::custom("invalid refresh idempotency key"))
    }
}

impl JsonSchema for RefreshIdempotencyKey {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("RefreshIdempotencyKey")
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({
            "type": "string",
            "minLength": 43,
            "maxLength": 43,
            "pattern": "^[A-Za-z0-9_-]{43}$",
            "description": "Canonical base64url-no-pad encoding of exactly 32 random bytes."
        })
    }
}
secret_string!(
    /// Opaque website session secret.
    WebSessionToken
);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmptyRequestV1 {
    pub version: Version1,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmptyResponseV1 {
    pub version: Version1,
}

/// Stable developer-facing failure. Clients branch on `code`, never `message`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeveloperErrorV1 {
    pub code: ErrorCode,
    pub message: String,
    /// Relative delay, independent of device clock skew. Absent on older hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_millis: Option<u64>,
}

impl fmt::Debug for DeveloperErrorV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeveloperErrorV1")
            .field("code", &self.code)
            .field("message", &"<redacted>")
            .finish()
    }
}

/// Append-only machine-readable failure token. Unknown valid tokens remain
/// failures and can be preserved by older clients.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ErrorCode(String);

impl ErrorCode {
    pub const INVALID_REQUEST: &'static str = "invalid_request";
    pub const UNAUTHENTICATED: &'static str = "unauthenticated";
    pub const PERMISSION_DENIED: &'static str = "permission_denied";
    pub const PROFILE_OFFLINE_ONLY_CONFIRMATION_REQUIRED: &'static str =
        "profile_offline_only_confirmation_required";
    pub const FRESH_GOOGLE_AUTHENTICATION_REQUIRED: &'static str =
        "fresh_google_authentication_required";
    pub const FRESH_AUTHENTICATION_REQUIRED: &'static str = "fresh_authentication_required";
    pub const CAPABILITY_UNAVAILABLE: &'static str = "capability_unavailable";
    pub const RATE_LIMITED: &'static str = "rate_limited";
    pub const FLOW_EXPIRED: &'static str = "flow_expired";
    pub const FLOW_CONSUMED: &'static str = "flow_consumed";
    pub const PROVIDER_REJECTED: &'static str = "provider_rejected";
    pub const IDENTITY_COLLISION: &'static str = "identity_collision";
    pub const PROFILE_ALREADY_BOUND: &'static str = "profile_already_bound";
    pub const PROFILE_DELETED: &'static str = "profile_deleted";
    pub const PROFILE_LIFECYCLE_STALE: &'static str = "profile_lifecycle_stale";
    pub const PROFILE_RESTORE_SOURCE_REQUIRED: &'static str = "profile_restore_source_required";
    pub const DEVICE_ALREADY_BOUND: &'static str = "device_already_bound";
    pub const CONFLICT: &'static str = "conflict";
    pub const NOT_FOUND: &'static str = "not_found";
    pub const SERVER_UNAVAILABLE: &'static str = "server_unavailable";

    pub fn parse(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        let mut chars = value.chars();
        let first = chars.next()?;
        if value.len() > 64
            || !first.is_ascii_lowercase()
            || !chars.all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
            })
        {
            return None;
        }
        Some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ErrorCode").field(&self.0).finish()
    }
}

impl Serialize for ErrorCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).ok_or_else(|| de::Error::custom("invalid account error code"))
    }
}

impl JsonSchema for ErrorCode {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("AccountErrorCode")
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": 64,
            "pattern": "^[a-z][a-z0-9_]*$",
            "description": "Stable append-only error token; unknown valid tokens remain errors."
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SymbolicReturnTarget {
    Account,
    Releases,
    DownloadWindows,
    DownloadMacos,
    // The website's app connection consent page (Cloud MCP TECHNICAL.final
    // §7.3, §9). A plain comment, not a doc comment: a described variant
    // would split the generated schema's flat `enum` into a `oneOf`.
    ConnectionAuthorize,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GoogleWebStartRequestV1 {
    pub version: Version1,
    pub return_target: SymbolicReturnTarget,
    pub locale: String,
    /// Opaque BFF correlation value, sealed into the flow and returned only to
    /// the configured fixed BFF callback.
    pub client_state: OAuthState,
}
impl fmt::Debug for GoogleWebStartRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoogleWebStartRequestV1")
            .field("return_target", &self.return_target)
            .field("correlation_and_locale", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthStartResponseV1 {
    pub version: Version1,
    pub flow_id: AuthFlowId,
    pub authorization_url: String,
    pub expires_at_millis: i64,
}

impl fmt::Debug for AuthStartResponseV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthStartResponseV1")
            .field("version", &self.version)
            .field("authorization_url", &"<redacted>")
            .field("expires_at_millis", &self.expires_at_millis)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GoogleCallbackRequestV1 {
    pub version: Version1,
    pub code: Option<AuthorizationCode>,
    pub state: OAuthState,
    pub provider_error: Option<ProviderCallbackFailureV1>,
}

impl fmt::Debug for GoogleCallbackRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GoogleCallbackRequestV1(<redacted>)")
    }
}

/// Apple's `form_post` callback. The same correlation fields as Google's,
/// plus `user`: the JSON Apple sends, unsigned and only on a person's first
/// authorization, carrying the name they chose to share. It is a display-name
/// seed and nothing else.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppleCallbackRequestV1 {
    pub version: Version1,
    pub code: Option<AuthorizationCode>,
    pub state: OAuthState,
    pub provider_error: Option<ProviderCallbackFailureV1>,
    pub user: Option<String>,
}

impl AppleCallbackRequestV1 {
    /// The provider-neutral part, which is what the flow engine consumes.
    pub fn into_parts(self) -> (GoogleCallbackRequestV1, Option<String>) {
        (
            GoogleCallbackRequestV1 {
                version: self.version,
                code: self.code,
                state: self.state,
                provider_error: self.provider_error,
            },
            self.user,
        )
    }
}

impl fmt::Debug for AppleCallbackRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AppleCallbackRequestV1(<redacted>)")
    }
}

/// Callback completion is machine-readable even when verified-email collision
/// policy requires a second, exact-account authentication. The collision flow
/// is newly minted by identity resolution; it is never confused with the
/// already consumed OAuth flow.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum GoogleCallbackResponseV1 {
    Cancelled {
        version: Version1,
        web_return: Option<WebReturnV1>,
        native_return: Option<NativeLoopbackReturnV1>,
    },
    ConfirmationRequired {
        version: Version1,
        confirmation: AccountCreationConfirmationV1,
        web_return: Option<WebReturnV1>,
        native_return: Option<NativeLoopbackReturnV1>,
    },
    Completed {
        version: Version1,
        grant: OneUseGrant,
        return_target: SymbolicReturnTarget,
        expires_at_millis: i64,
        web_return: Option<WebReturnV1>,
        native_return: Option<NativeLoopbackReturnV1>,
    },
    CollisionRequired {
        version: Version1,
        collision_flow_id: AuthFlowId,
        reauthentication: Box<AuthStartResponseV1>,
        provider: AuthProvider,
        prompt: CollisionPromptV1,
        web_return: Option<WebReturnV1>,
        native_return: Option<NativeLoopbackReturnV1>,
        reauthentication_web_return: Option<WebReturnV1>,
        reauthentication_native_return: Option<NativeLoopbackReturnV1>,
    },
    ProviderRejected {
        version: Version1,
        code: ProviderCallbackFailureV1,
        web_return: Option<WebReturnV1>,
        native_return: Option<NativeLoopbackReturnV1>,
    },
    AccountMismatch {
        version: Version1,
        web_return: Option<WebReturnV1>,
        native_return: Option<NativeLoopbackReturnV1>,
    },
}

impl fmt::Debug for GoogleCallbackResponseV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfirmationRequired { .. } => {
                f.write_str("GoogleCallbackResponseV1::ConfirmationRequired(<redacted>)")
            }
            Self::Cancelled { .. } => {
                f.write_str("GoogleCallbackResponseV1::Cancelled(<redacted>)")
            }
            Self::Completed { .. } => {
                f.write_str("GoogleCallbackResponseV1::Completed(<redacted>)")
            }
            Self::CollisionRequired { .. } => {
                f.write_str("GoogleCallbackResponseV1::CollisionRequired(<redacted>)")
            }
            Self::ProviderRejected { .. } => {
                f.write_str("GoogleCallbackResponseV1::ProviderRejected(<redacted>)")
            }
            Self::AccountMismatch { .. } => {
                f.write_str("GoogleCallbackResponseV1::AccountMismatch(<redacted>)")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCallbackFailureV1 {
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WebReturnV1 {
    pub client_state: OAuthState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NativeLoopbackReturnV1 {
    pub port: u16,
    pub client_state: OAuthState,
    /// Deliver to the fixed verified App Link instead of `127.0.0.1:port`.
    /// A phone has no loopback a browser can reliably reach, so it asks for
    /// this at start and `port` is then zero.
    #[serde(default)]
    pub app_link: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CollisionPromptV1 {
    SelectAccount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WebExchangeRequestV1 {
    pub version: Version1,
    pub grant: OneUseGrant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WebExchangeResponseV1 {
    pub version: Version1,
    pub session: WebSessionToken,
    pub expires_at_millis: i64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GoogleNativeStartRequestV1 {
    pub version: Version1,
    pub descriptor: SignedDeviceDescriptorV1,
    pub platform: DevicePlatform,
    pub app_version: String,
    #[serde(default)]
    pub enrollment_protocol_version: Option<u32>,
    #[serde(default)]
    pub peer_protocol_version: Option<u32>,
    /// Zero exactly when `app_link_return` is set.
    pub callback_port: u16,
    pub client_state: OAuthState,
    pub pkce_challenge: String,
    pub locale: String,
    /// Return through the website's verified App Link rather than a loopback
    /// port: how a phone completes a browser sign-in (RFC 8252 §7.2).
    #[serde(default)]
    pub app_link_return: bool,
}

impl fmt::Debug for GoogleNativeStartRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoogleNativeStartRequestV1")
            .field("version", &self.version)
            .field("descriptor", &self.descriptor)
            .field("platform", &self.platform)
            .field("callback_port", &self.callback_port)
            .field("app_version", &"<redacted>")
            .field("client_state", &"<redacted>")
            .field("pkce_challenge", &"<redacted>")
            .field("locale", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GoogleNativeTokenStartRequestV1 {
    pub version: Version1,
    pub descriptor: SignedDeviceDescriptorV1,
    pub platform: DevicePlatform,
    pub app_version: String,
    pub enrollment_protocol_version: Option<u32>,
    pub peer_protocol_version: Option<u32>,
    pub client_state: OAuthState,
    pub pkce_challenge: String,
    pub locale: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GoogleNativeTokenStartResponseV1 {
    pub version: Version1,
    pub flow_id: AuthFlowId,
    pub flow_state: OAuthState,
    pub nonce: OAuthState,
    pub expires_at_millis: i64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GoogleNativeTokenCompleteRequestV1 {
    pub version: Version1,
    pub flow_id: AuthFlowId,
    pub flow_state: OAuthState,
    pub client_state: OAuthState,
    pub pkce_verifier: PkceVerifier,
    pub id_token: GoogleIdToken,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GoogleNativeTokenCompleteResponseV1 {
    ConfirmationRequired {
        version: Version1,
        confirmation: AccountCreationConfirmationV1,
    },
    Cancelled {
        version: Version1,
    },
    ExchangeReady {
        version: Version1,
        flow_id: AuthFlowId,
        grant: OneUseGrant,
        expires_at_millis: i64,
    },
    CollisionRequired {
        version: Version1,
        reauthentication: GoogleNativeTokenStartResponseV1,
    },
}

/// An opaque server-owned creation decision. Verified provider claims never
/// cross this boundary; the same original client must redeem the authority.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountCreationConfirmationV1 {
    pub authority: OneUseGrant,
    pub expires_at_millis: i64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountCreationRequestV1 {
    pub version: Version1,
    pub authority: OneUseGrant,
    pub client_state: OAuthState,
    pub pkce_verifier: Option<PkceVerifier>,
}

/// A race returns the decision which actually committed, including a confirmed
/// continuation when cancellation arrived too late. It never claims erasure.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccountCreationResponseV1 {
    Browser {
        callback: GoogleCallbackResponseV1,
    },
    Native {
        completion: GoogleNativeTokenCompleteResponseV1,
    },
    Cancelled {
        version: Version1,
    },
}

macro_rules! redacted_native_token_debug {
    ($($name:ty),+ $(,)?) => {$(
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }
    )+};
}
redacted_native_token_debug!(
    AccountCreationConfirmationV1,
    AccountCreationRequestV1,
    AccountCreationResponseV1,
    GoogleNativeTokenStartRequestV1,
    GoogleNativeTokenStartResponseV1,
    GoogleNativeTokenCompleteRequestV1,
    GoogleNativeTokenCompleteResponseV1,
);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NativeExchangeRequestV1 {
    #[serde(default)]
    pub device_ownership_proof: Option<String>,
    pub version: Version1,
    pub flow_id: AuthFlowId,
    pub grant: OneUseGrant,
    pub client_state: OAuthState,
    pub pkce_verifier: PkceVerifier,
}
impl fmt::Debug for NativeExchangeRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeExchangeRequestV1(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TokenPairV1 {
    pub version: Version1,
    pub session_id: AccountSessionId,
    pub access_token: AccessToken,
    pub access_expires_at_millis: i64,
    pub refresh_token: RefreshToken,
    pub refresh_idle_expires_at_millis: i64,
    pub refresh_absolute_expires_at_millis: i64,
}
impl fmt::Debug for TokenPairV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenPairV1")
            .field("version", &self.version)
            .field("credentials", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NativeExchangeResponseV1 {
    pub version: Version1,
    pub tokens: TokenPairV1,
    pub account: AccountSummaryV1,
}
impl fmt::Debug for NativeExchangeResponseV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeExchangeResponseV1(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RefreshRequestV1 {
    pub version: Version1,
    pub session_id: AccountSessionId,
    pub refresh_token: RefreshToken,
    pub idempotency_key: RefreshIdempotencyKey,
}
impl fmt::Debug for RefreshRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RefreshRequestV1(<redacted>)")
    }
}

pub type RefreshResponseV1 = TokenPairV1;

/// Read-only ownership observation authenticated by the current native refresh
/// secret. It issues no token, extends no expiry, and never accepts a previous
/// secret. Later operations must authenticate normally.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativeSessionInspectRequestV1 {
    pub version: Version1,
    pub session_id: AccountSessionId,
    /// Canonical base64url without padding of exactly 32 current secret bytes.
    #[schemars(with = "String", length(min = 43, max = 43))]
    pub refresh_token: RefreshToken,
    #[schemars(with = "String")]
    pub device_id: DeviceId,
}
impl fmt::Debug for NativeSessionInspectRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeSessionInspectRequestV1(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LogoutRequestV1 {
    pub version: Version1,
    pub session_id: AccountSessionId,
}
impl fmt::Debug for LogoutRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LogoutRequestV1(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionSummaryV1 {
    pub version: Version1,
    pub session_id: AccountSessionId,
    pub koru_identity_id: KoruIdentityId,
    /// The exact Auth Identity that authenticated this session. This is an
    /// opaque identifier, not an email or provider subject.
    pub auth_identity_id: AuthIdentityId,
    #[schemars(with = "Option<String>")]
    pub device_id: Option<DeviceId>,
    pub expires_at_millis: i64,
    pub recent_auth_at_millis: i64,
}
impl fmt::Debug for SessionSummaryV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionSummaryV1")
            .field("device_present", &self.device_id.is_some())
            .field("expires_at_millis", &self.expires_at_millis)
            .field("recent_auth_at_millis", &self.recent_auth_at_millis)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthIdentitySummaryV1 {
    pub version: Version1,
    pub auth_identity_id: AuthIdentityId,
    pub provider: AuthProvider,
    pub verified_email_snapshot: Option<String>,
}

impl fmt::Debug for AuthIdentitySummaryV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthIdentitySummaryV1")
            .field("provider", &self.provider)
            .field(
                "verified_email_present",
                &self.verified_email_snapshot.is_some(),
            )
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountSummaryV1 {
    pub version: Version1,
    pub koru_identity_id: KoruIdentityId,
    pub display_name: String,
    pub auth_identities: Vec<AuthIdentitySummaryV1>,
    pub terms_version_at_creation: String,
    pub created_at_millis: i64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateAccountRequestV1 {
    pub version: Version1,
    pub display_name: String,
}
impl fmt::Debug for UpdateAccountRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpdateAccountRequestV1")
            .field("display_name_present", &!self.display_name.is_empty())
            .finish()
    }
}
impl fmt::Debug for AccountSummaryV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountSummaryV1")
            .field("auth_identity_count", &self.auth_identities.len())
            .field("created_at_millis", &self.created_at_millis)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JwksResponseV1 {
    pub version: Version1,
    /// Access-token verification keys. Existing consumers keep this trust
    /// domain and never treat attestation keys as login keys.
    pub keys: Vec<JwkV1>,
    /// Independently rotated device-attestation verification keys.
    #[serde(default)]
    pub attestation_keys: Vec<JwkV1>,
    /// The regional token/capability verification keys: what a device checks
    /// its own sync credential against before it dials a worker. Public keys
    /// only, in a third trust domain; a deployment without capability signing
    /// publishes none.
    #[serde(default)]
    pub capability_keys: Vec<JwkV1>,
}

/// Public Ed25519 verification key in RFC 7517/8037 form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JwkV1 {
    pub kty: String,
    pub crv: String,
    #[serde(rename = "use")]
    pub key_use: String,
    pub alg: String,
    pub kid: String,
    pub x: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum LinkGoogleIntentV1 {
    AddGoogle {
        profile_id: ProfileId,
    },
    Rebind {
        profile_id: ProfileId,
        erased_koru_identity_id: KoruIdentityId,
    },
}
impl fmt::Debug for LinkGoogleIntentV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AddGoogle { .. } => "LinkGoogleIntentV1::AddGoogle(<redacted>)",
            Self::Rebind { .. } => "LinkGoogleIntentV1::Rebind(<redacted>)",
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "delivery", rename_all = "snake_case")]
pub enum LinkCallbackDeliveryV1 {
    Web {
        client_state: OAuthState,
    },
    Native {
        callback_port: u16,
        client_state: OAuthState,
    },
}

impl std::fmt::Debug for LinkCallbackDeliveryV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Web { .. } => formatter.write_str("LinkCallbackDeliveryV1::Web(<redacted>)"),
            Self::Native { callback_port, .. } => formatter
                .debug_struct("LinkCallbackDeliveryV1::Native")
                .field("callback_port", callback_port)
                .field("client_state", &"<redacted>")
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LinkGoogleStartRequestV1 {
    pub version: Version1,
    pub intent: LinkGoogleIntentV1,
    pub callback: LinkCallbackDeliveryV1,
    pub locale: String,
}
impl fmt::Debug for LinkGoogleStartRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LinkGoogleStartRequestV1(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LinkGoogleContinueRequestV1 {
    pub version: Version1,
    pub flow_id: AuthFlowId,
    pub proof_grant: OneUseGrant,
}
impl fmt::Debug for LinkGoogleContinueRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LinkGoogleContinueRequestV1(<redacted>)")
    }
}

/// Opaque, one-use authorization candidate produced by a verified provider
/// callback. Its contents are server-owned and must never be logged.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct AddGoogleCandidateId(pub String);

impl fmt::Debug for AddGoogleCandidateId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AddGoogleCandidateId(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AddGoogleCandidateV1 {
    pub version: Version1,
    pub candidate_id: AddGoogleCandidateId,
    pub profile_id: ProfileId,
    /// Display-only provider-verified email. It is never an authorization key.
    pub verified_email: String,
    pub expires_at_millis: i64,
}

impl fmt::Debug for AddGoogleCandidateV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AddGoogleCandidateV1(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfirmAddGoogleRequestV1 {
    pub version: Version1,
    pub candidate_id: AddGoogleCandidateId,
    /// The Profile selected at confirmation time. A changed selection must not
    /// silently authorize the Profile frozen into the candidate.
    pub profile_id: ProfileId,
}

impl fmt::Debug for ConfirmAddGoogleRequestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ConfirmAddGoogleRequestV1(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmAddGoogleStatusV1 {
    Added,
    AlreadyAuthorized,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfirmAddGoogleResponseV1 {
    pub version: Version1,
    pub koru_identity_id: KoruIdentityId,
    pub profile_id: ProfileId,
    pub status: ConfirmAddGoogleStatusV1,
}

impl fmt::Debug for ConfirmAddGoogleResponseV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfirmAddGoogleResponseV1")
            .field("status", &self.status)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CancelAddGoogleRequestV1 {
    pub version: Version1,
    pub candidate_id: AddGoogleCandidateId,
    pub profile_id: ProfileId,
}

impl fmt::Debug for CancelAddGoogleRequestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CancelAddGoogleRequestV1(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CancelAddGoogleStatusV1 {
    Cancelled,
    AlreadyCancelled,
    AlreadyConfirmed,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CancelAddGoogleResponseV1 {
    pub version: Version1,
    pub profile_id: ProfileId,
    pub status: CancelAddGoogleStatusV1,
}

impl fmt::Debug for CancelAddGoogleResponseV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancelAddGoogleResponseV1")
            .field("status", &self.status)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoveProfileAuthorizationRequestV1 {
    pub version: Version1,
    pub auth_identity_id: AuthIdentityId,
    pub confirm_profile_offline_only: bool,
}

impl fmt::Debug for RemoveProfileAuthorizationRequestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoveProfileAuthorizationRequestV1(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProfileAuthorizationRemovalStatusV1 {
    Removed,
    AlreadyRemoved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProfileAuthorizationRemovalResponseV1 {
    pub version: Version1,
    pub status: ProfileAuthorizationRemovalStatusV1,
    pub profile_offline_only: bool,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProfileSummaryV1 {
    pub version: Version1,
    /// Canonical hosted owner used only by targeted acknowledgements.
    pub koru_identity_id: KoruIdentityId,
    pub profile_id: ProfileId,
    /// Discovery snapshot only; local Profile CRDT data is authoritative.
    pub name_snapshot: Option<String>,
    pub locale_snapshot: Option<String>,
    /// Auth Identities authorized for this Profile. Filled by Profile listing;
    /// mutation responses may return an empty list and clients then refresh.
    #[serde(default)]
    pub auth_identities: Vec<AuthIdentitySummaryV1>,
    /// Current hosted compare-and-set revision. Absence in an older response is
    /// the initial active revision.
    #[serde(default)]
    pub lifecycle_revision: ProfileLifecycleRevision,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeleteProfileRequestV1 {
    pub version: Version1,
    pub operation_id: ProfileLifecycleMutationId,
    pub expected_lifecycle_revision: ProfileLifecycleRevision,
}

impl fmt::Debug for DeleteProfileRequestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DeleteProfileRequestV1(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RestoreProfileRequestV1 {
    pub version: Version1,
    pub operation_id: ProfileLifecycleMutationId,
    pub expected_lifecycle_revision: ProfileLifecycleRevision,
    pub name_snapshot: Option<String>,
    pub locale_snapshot: Option<String>,
}

impl fmt::Debug for RestoreProfileRequestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RestoreProfileRequestV1(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResolveProfileLifecyclesRequestV1 {
    pub version: Version1,
    pub profile_ids: Vec<ProfileId>,
}

impl fmt::Debug for ResolveProfileLifecyclesRequestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolveProfileLifecyclesRequestV1")
            .field("profile_count", &self.profile_ids.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProfileLifecycleMutationStatusV1 {
    Applied,
    ExactRetry,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProfileLifecycleResponseV1 {
    pub version: Version1,
    pub status: ProfileLifecycleMutationStatusV1,
    pub lifecycle: ProfileLifecycle,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResolveProfileLifecyclesResponseV1 {
    pub version: Version1,
    pub lifecycles: Vec<ProfileLifecycle>,
}

impl fmt::Debug for ProfileSummaryV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileSummaryV1")
            .field("name_snapshot_present", &self.name_snapshot.is_some())
            .field("locale_snapshot_present", &self.locale_snapshot.is_some())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProfileListResponseV1 {
    pub version: Version1,
    pub profiles: Vec<ProfileSummaryV1>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreateProfileRequestV1 {
    pub version: Version1,
    pub profile_id: ProfileId,
    pub name_snapshot: String,
    pub locale_snapshot: Option<String>,
}

impl fmt::Debug for CreateProfileRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateProfileRequestV1")
            .field("metadata", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttachProfileRequestV1 {
    pub version: Version1,
    pub profile_id: ProfileId,
    pub name_snapshot: Option<String>,
    pub locale_snapshot: Option<String>,
}

impl fmt::Debug for AttachProfileRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachProfileRequestV1")
            .field("metadata", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RebindProfileRequestV1 {
    pub version: Version1,
    pub profile_id: ProfileId,
    pub erased_koru_identity_id: KoruIdentityId,
    pub fresh_auth_grant: OneUseGrant,
    pub explicit_rebind: bool,
    pub name_snapshot: Option<String>,
    pub locale_snapshot: Option<String>,
}

impl fmt::Debug for RebindProfileRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RebindProfileRequestV1")
            .field("explicit_rebind", &self.explicit_rebind)
            .field("proof_and_metadata", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SignedDeviceDescriptorV1 {
    pub version: Version1,
    #[schemars(with = "String")]
    pub device_id: DeviceId,
    pub display_name: String,
    pub signing_public_key: Vec<u8>,
    pub endpoint_id: Vec<u8>,
    pub created_at_millis: i64,
    pub endpoint_binding_signature: Vec<u8>,
}

impl fmt::Debug for SignedDeviceDescriptorV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignedDeviceDescriptorV1")
            .field("created_at_millis", &self.created_at_millis)
            .field("descriptor_material", &"<redacted>")
            .finish()
    }
}

impl TryFrom<DeviceDescriptorV1> for SignedDeviceDescriptorV1 {
    type Error = &'static str;

    fn try_from(value: DeviceDescriptorV1) -> Result<Self, Self::Error> {
        value
            .verify()
            .map_err(|_| "device descriptor version or signature is invalid")?;
        Ok(Self {
            version: Version1,
            device_id: value.device_id,
            display_name: value.display_name,
            signing_public_key: value.signing_public_key.as_bytes().to_vec(),
            endpoint_id: value.endpoint_id.as_bytes().to_vec(),
            created_at_millis: value.created_at_millis,
            endpoint_binding_signature: value.endpoint_binding_signature.as_bytes().to_vec(),
        })
    }
}

impl TryFrom<SignedDeviceDescriptorV1> for DeviceDescriptorV1 {
    type Error = &'static str;

    fn try_from(value: SignedDeviceDescriptorV1) -> Result<Self, Self::Error> {
        let signing_public_key: [u8; 32] = value
            .signing_public_key
            .try_into()
            .map_err(|_| "device signing public key must be 32 bytes")?;
        let endpoint_id: [u8; 32] = value
            .endpoint_id
            .try_into()
            .map_err(|_| "device endpoint id must be 32 bytes")?;
        let signature: [u8; 64] = value
            .endpoint_binding_signature
            .try_into()
            .map_err(|_| "device endpoint binding signature must be 64 bytes")?;
        let descriptor = DeviceDescriptorV1 {
            version: 1,
            device_id: value.device_id,
            display_name: value.display_name,
            signing_public_key: DevicePublicKey::from_bytes(signing_public_key),
            endpoint_id: ReplicationEndpointId::from_bytes(endpoint_id),
            created_at_millis: value.created_at_millis,
            endpoint_binding_signature: SignatureBytes::from_bytes(signature),
        };
        descriptor
            .verify()
            .map_err(|_| "device descriptor signature is invalid")?;
        Ok(descriptor)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RegisterDeviceRequestV1 {
    #[serde(default)]
    pub device_ownership_proof: Option<String>,
    pub version: Version1,
    pub descriptor: SignedDeviceDescriptorV1,
    pub platform: DevicePlatform,
    pub app_version: String,
    /// Enrollment protocol spoken by this build. Old clients omit it and are
    /// deliberately not returned as automatic-enrollment candidates.
    #[serde(default)]
    pub enrollment_protocol_version: Option<u32>,
    #[serde(default)]
    pub peer_protocol_version: Option<u32>,
}

/// Opaque, bounded challenge issued after authenticating the registration flow.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceOwnershipChallengeResponseV1 {
    pub version: Version1,
    pub challenge: String,
}

impl fmt::Debug for DeviceOwnershipChallengeResponseV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceOwnershipChallengeResponseV1(<redacted>)")
    }
}

impl fmt::Debug for RegisterDeviceRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisterDeviceRequestV1")
            .field("descriptor", &self.descriptor)
            .field("platform", &self.platform)
            .field("app_version", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceSummaryV1 {
    pub version: Version1,
    #[schemars(with = "String")]
    pub device_id: DeviceId,
    pub display_name: String,
    pub platform: DevicePlatform,
    pub app_version: String,
    #[serde(default)]
    pub enrollment_protocol_version: Option<u32>,
    #[serde(default)]
    pub peer_protocol_version: Option<u32>,
    pub endpoint_id_hex: String,
    pub profile_states: Vec<DeviceProfileSummaryV1>,
    pub last_seen_at_millis: i64,
    pub revoked: bool,
}

impl fmt::Debug for DeviceSummaryV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceSummaryV1")
            .field("platform", &self.platform)
            .field("profile_count", &self.profile_states.len())
            .field("last_seen_at_millis", &self.last_seen_at_millis)
            .field("revoked", &self.revoked)
            .field("discovery_metadata", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceProfileSummaryV1 {
    pub version: Version1,
    pub profile_id: ProfileId,
    pub state: DeviceProfileState,
}
impl fmt::Debug for DeviceProfileSummaryV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceProfileSummaryV1")
            .field("state", &self.state)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceListResponseV1 {
    pub version: Version1,
    pub devices: Vec<DeviceSummaryV1>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnrollmentCandidateV1 {
    pub version: Version1,
    #[schemars(with = "String")]
    pub device_id: DeviceId,
    pub signing_public_key: Vec<u8>,
    pub endpoint_id: Vec<u8>,
    pub platform: DevicePlatform,
    pub app_version: String,
    pub enrollment_protocol_version: u32,
    pub peer_protocol_version: u32,
}

impl fmt::Debug for EnrollmentCandidateV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnrollmentCandidateV1")
            .field("platform", &self.platform)
            .field(
                "enrollment_protocol_version",
                &self.enrollment_protocol_version,
            )
            .field("peer_protocol_version", &self.peer_protocol_version)
            .field("discovery_material", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnrollmentCandidatesResponseV1 {
    pub version: Version1,
    pub candidates: Vec<EnrollmentCandidateV1>,
    pub truncated: bool,
}

impl fmt::Debug for EnrollmentCandidatesResponseV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnrollmentCandidatesResponseV1")
            .field("candidate_count", &self.candidates.len())
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// Fixed discriminator of a Cloud discovery candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudWorkspaceCandidateKindV1 {
    Cloud,
}

/// One currently authorized Profile/Workspace eligibility pair. No name,
/// local-only sibling, route, descriptor or credential appears here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudWorkspaceCandidateV1 {
    pub kind: CloudWorkspaceCandidateKindV1,
    pub profile_id: ProfileId,
    pub workspace_id: WorkspaceId,
    #[schemars(with = "String")]
    pub workspace_incarnation_id: uuid::Uuid,
    pub authorization_version: i64,
    pub membership_epoch: i64,
    pub cloud_access_epoch: i64,
}

/// The last returned key of a page, echoed back as `after_*` query values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudWorkspaceCursorV1 {
    pub profile_id: ProfileId,
    pub workspace_id: WorkspaceId,
}

/// A bounded discovery page. `next` is explicitly null on the last page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudWorkspacePageV1 {
    pub version: Version1,
    pub workspaces: Vec<CloudWorkspaceCandidateV1>,
    pub next: Option<CloudWorkspaceCursorV1>,
}

/// Select one exact discovery eligibility pair for the current native device.
/// The Profile comes from the path; every other identity comes from the
/// session, never from this body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudProfileSelectionRequestV1 {
    pub version: Version1,
    #[serde(deserialize_with = "canonical_v4_workspace_id")]
    pub workspace_id: WorkspaceId,
}

pub(crate) fn canonical_v4_workspace_id<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<WorkspaceId, D::Error> {
    let value = String::deserialize(d)?;
    let id = uuid::Uuid::parse_str(&value).map_err(de::Error::custom)?;
    if id.to_string() != value
        || id.get_version() != Some(uuid::Version::Random)
        || id.get_variant() != uuid::Variant::RFC4122
    {
        return Err(de::Error::custom("invalid canonical v4 Workspace UUID"));
    }
    Ok(WorkspaceId::from_uuid(id))
}

/// Identity-only bootstrap facts for one authorized Workspace, including the
/// manifest's original Store document id retained by the Cloud catalog.
/// Nothing here names, describes or authorizes content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudWorkspaceDescriptorV1 {
    pub version: Version1,
    pub profile_id: ProfileId,
    pub workspace_id: WorkspaceId,
    #[schemars(with = "String")]
    pub workspace_incarnation_id: uuid::Uuid,
    #[schemars(with = "String")]
    pub logical_replica_id: uuid::Uuid,
    #[schemars(with = "String")]
    pub lineage_id: uuid::Uuid,
    #[schemars(with = "String")]
    pub manifest_document_id: uuid::Uuid,
    pub authorization_version: i64,
    pub membership_epoch: i64,
    pub cloud_access_epoch: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudProfileSelectionStateV1 {
    /// Completed admission for this device and Profile. It is not possession
    /// of Profile bytes or complete sync; local CRDT inventory is that evidence.
    Selected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudProfileSelectionResponseV1 {
    pub version: Version1,
    pub state: CloudProfileSelectionStateV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceAttestationRequestV1 {
    pub version: Version1,
}

/// Public signed evidence relayed to one enrollment peer. Although it is not
/// a credential, Debug stays structural so logs cannot accidentally become a
/// durable copy of the complete enrollment claim.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceAttestationEvidenceV1 {
    pub compact: String,
}

impl fmt::Debug for DeviceAttestationEvidenceV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DeviceAttestationEvidenceV1(<signed-evidence>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceAttestationResponseV1 {
    pub version: Version1,
    pub attestation: DeviceAttestationEvidenceV1,
    pub expires_at_millis: i64,
}

impl fmt::Debug for DeviceAttestationResponseV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceAttestationResponseV1")
            .field("expires_at_millis", &self.expires_at_millis)
            .field("attestation", &"<signed-evidence>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProvisionDeviceRequestV1 {
    pub version: Version1,
    pub attestation: DeviceAttestationEvidenceV1,
    pub completion_receipt: EnrollmentCompletionReceiptV1,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct EnrollmentCompletionReceiptV1(pub String);

impl fmt::Debug for EnrollmentCompletionReceiptV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnrollmentCompletionReceiptV1(<signed-receipt>)")
    }
}

impl fmt::Debug for ProvisionDeviceRequestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProvisionDeviceRequestV1(<signed-evidence>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProvisioningResultV1 {
    Provisioned,
    ExactRetry,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProvisionDeviceResponseV1 {
    pub version: Version1,
    pub result: ProvisioningResultV1,
}

// ------------------------------------------------------------ email sign-in

/// Begin proving an address. **The response is identical for every address**,
/// which is the whole of D8: a stranger typing addresses into the sign-up box
/// must not be able to learn whether any of them has a Koru account.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmailStartRequestV1 {
    pub version: Version1,
    pub address: String,
    pub locale: String,
    pub client_state: OAuthState,
    /// The native flow this address belongs to, when the app started one.
    ///
    /// A native sign-in begins in the app, before anybody has typed an
    /// address: the device descriptor, callback port and PKCE challenge are
    /// sealed then. This attaches the address to that flow instead of opening
    /// a second one, which is what keeps the device bound to the account that
    /// is about to be created.
    #[serde(default)]
    pub flow_id: Option<AuthFlowId>,
}

impl fmt::Debug for EmailStartRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The address is the request; there is nothing here a log may keep.
        f.write_str("EmailStartRequestV1(<redacted>)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmailStartResponseV1 {
    pub version: Version1,
    pub flow_id: AuthFlowId,
    pub expires_at_millis: i64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmailVerifyRequestV1 {
    pub version: Version1,
    pub flow_id: AuthFlowId,
    /// **The address again, and it is checked against the challenge.**
    ///
    /// Every step after `start` re-supplies it, and the server refuses unless
    /// its keyed digest equals the one the code was sent to. That is what lets
    /// a native flow exist before an address does, and it means no step has to
    /// store the address anywhere a later step could disagree with.
    pub address: String,
    pub code: String,
}

impl fmt::Debug for EmailVerifyRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmailVerifyRequestV1")
            .field("flow_id", &self.flow_id)
            .field("code", &"<redacted>")
            .finish()
    }
}

/// What a code came back as.
///
/// The two "already known" outcomes are D8 arriving: they are the first moment
/// Koru will say that an address is known, and they are reached only by
/// somebody who has just proved they read that mailbox.
///
/// **They are two outcomes and not one because they send the person to
/// different places.** One has a password already and should sign in; the other
/// has a Google account and has no password to type. A single outcome meant the
/// page told somebody who had never used Google to press the Google button.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum EmailVerifyResponseV1 {
    /// Proved, and no account exists at this address. Choose a password next.
    PasswordRequired {
        version: Version1,
    },
    /// Proved, and this address already has a Koru password. Sign in with it.
    AlreadyRegistered {
        version: Version1,
    },
    /// Proved, and this address already signs in with Google. Increment E1
    /// turns this into a journey; until then it is an honest dead end rather
    /// than a second account.
    AlreadyLinkedToGoogle {
        version: Version1,
    },
    CodeIncorrect {
        version: Version1,
        attempts_left: i32,
    },
    CodeExpired {
        version: Version1,
    },
    /// The challenge is dead. A new code must be requested; this is terminal,
    /// not a slower retry.
    AttemptsExhausted {
        version: Version1,
    },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmailCompleteRequestV1 {
    pub version: Version1,
    pub flow_id: AuthFlowId,
    /// Checked against the challenge, as at `verify`.
    pub address: String,
    /// **The code again, not a token saying it was checked.**
    ///
    /// Re-checking here means a password can only be set by somebody who still
    /// holds the code, rather than by anybody holding a flow id that was
    /// verified once. It also means no "proved" marker has to be stored, which
    /// would have been either a schema column or a resealed flow artifact --
    /// state existing only to remember something the caller can simply prove
    /// again.
    pub code: String,
    pub password: String,
    /// That the person accepted the terms. The *version* is the server's own
    /// configured one; a client-supplied version would let a caller record
    /// acceptance of terms nobody showed them.
    pub accepted_terms: bool,
}

impl fmt::Debug for EmailCompleteRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmailCompleteRequestV1")
            .field("flow_id", &self.flow_id)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Why a password was refused, in terms a surface can show *before* the next
/// attempt rather than as a bare rejection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum PasswordRefusedV1 {
    TooShort { required: u32 },
    TooLong { allowed: u32 },
    TooFewClasses { required: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum EmailCompleteResponseV1 {
    Completed {
        version: Version1,
        grant: OneUseGrant,
        expires_at_millis: i64,
    },
    PasswordRefused {
        version: Version1,
        refusal: PasswordRefusedV1,
    },
    /// Somebody finished a sign-up at this address first. Rare, and honest.
    AddressTaken { version: Version1 },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmailSignInRequestV1 {
    pub version: Version1,
    pub address: String,
    pub password: String,
    pub client_state: OAuthState,
    /// The native flow this sign-in belongs to, when the app started one.
    #[serde(default)]
    pub flow_id: Option<AuthFlowId>,
}

impl fmt::Debug for EmailSignInRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EmailSignInRequestV1(<redacted>)")
    }
}

/// **One rejection, whatever was wrong.** A response that distinguished "no
/// account here" from "wrong password" would answer the question D8 spends a
/// screen and an email refusing to answer -- and the constant-time verify in
/// `PasswordHashing::verify` exists so the *timing* does not answer it either.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum EmailSignInResponseV1 {
    Completed {
        version: Version1,
        grant: OneUseGrant,
        expires_at_millis: i64,
    },
    Rejected {
        version: Version1,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmailResendRequestV1 {
    pub version: Version1,
    pub flow_id: AuthFlowId,
    /// Checked against the challenge, so this button cannot be pointed at an
    /// address the flow was not started for.
    pub address: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum EmailResendResponseV1 {
    Sent {
        version: Version1,
    },
    /// The sixty-second cooldown, which is what stops this button being used
    /// to flood somebody else's inbox.
    TooSoon {
        version: Version1,
        retry_after_millis: i64,
    },
}

/// Begin an email sign-in from a native client.
///
/// The same shape as `GoogleNativeStartRequestV1`, and deliberately so: the
/// device descriptor, callback port, client state and PKCE challenge are
/// sealed into the flow here, and everything downstream of the exchange —
/// device registration, the ownership challenge, Profile selection,
/// enrollment — is then the Google path unchanged.
/// What a native client gets back when it opens an email flow.
///
/// **Its own type, not `AuthStartResponseV1`, because there is no URL.** That
/// type carries an `authorization_url` and must: a Google start exists to send
/// a browser somewhere. An email start sends nobody anywhere -- the app shows
/// the code screen itself -- so the field had no caller, and a response field
/// nobody reads is a puzzle for whoever arrives next (D22).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmailNativeStartResponseV1 {
    pub version: Version1,
    pub flow_id: AuthFlowId,
    pub expires_at_millis: i64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmailNativeStartRequestV1 {
    pub version: Version1,
    pub descriptor: SignedDeviceDescriptorV1,
    pub platform: DevicePlatform,
    pub app_version: String,
    #[serde(default)]
    pub enrollment_protocol_version: Option<u32>,
    #[serde(default)]
    pub peer_protocol_version: Option<u32>,
    pub client_state: OAuthState,
    pub pkce_challenge: String,
    pub locale: String,
}

impl fmt::Debug for EmailNativeStartRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmailNativeStartRequestV1")
            .field("platform", &self.platform)
            .field("secrets", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BeginWorkspaceCloudActivationRequestV1 {
    pub version: Version1,
    #[schemars(with = "String")]
    pub operation_id: uuid::Uuid,
    pub owner_profile_id: ProfileId,
    pub expected_authorization_version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceCloudActivationChallengeV1 {
    pub version: Version1,
    #[schemars(with = "String")]
    pub challenge_id: uuid::Uuid,
    #[schemars(with = "String")]
    pub workspace_id: WorkspaceId,
    pub signing_bytes: Vec<u8>,
    pub expires_at_millis: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompleteWorkspaceCloudActivationRequestV1 {
    pub version: Version1,
    #[schemars(with = "String")]
    pub challenge_id: uuid::Uuid,
    pub signature: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MutateWorkspaceCloudRequestV1 {
    pub version: Version1,
    #[schemars(with = "String")]
    pub operation_id: uuid::Uuid,
    pub owner_profile_id: ProfileId,
    pub expected_authorization_version: u64,
    #[schemars(with = "String")]
    pub expected_incarnation_id: uuid::Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceCloudLifecycleResponseV1 {
    pub version: Version1,
    pub status: WorkspaceLifecycleStatus,
    #[schemars(with = "String")]
    pub workspace_id: WorkspaceId,
    pub state: WorkspaceCloudState,
    #[schemars(with = "String")]
    pub incarnation_id: uuid::Uuid,
    pub membership_epoch: u64,
    pub cloud_access_epoch: u64,
    pub authorization_version: u64,
}
