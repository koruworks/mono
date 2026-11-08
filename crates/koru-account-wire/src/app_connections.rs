// SPDX-License-Identifier: MPL-2.0

//! App connections on the wire (Cloud MCP TECHNICAL.final §7, §9): the OAuth
//! authorization server's protocol documents, the person's consent and
//! connection routes, and the internal routes the hosted MCP server calls.
//!
//! The account-session routes (consent, the connections list and its edits)
//! are ordinary [`crate::ROUTES`] entries with versioned bodies. The OAuth
//! protocol routes are listed in [`OAUTH_ROUTES`] instead: their requests are
//! form-encoded or RFC-shaped JSON and their errors are RFC 6749's
//! `{error, error_description}`, not [`crate::DeveloperErrorV1`], so the
//! account dispatcher does not serve them. [`INTERNAL_ROUTES`] are served
//! only on the API's internal listener.

use koru_account_core::{AppConnectionId, DocumentId, KoruIdentityId, ProfileId, WorkspaceId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

use crate::{Method, Version1};

/// The one protected resource rung 1 serves (C19: US only).
pub const MCP_RESOURCE: &str = "https://mcp.koru.do/mcp";
/// The scopes `scopes_supported` lists, in order.
pub const OAUTH_SCOPES: [&str; 2] = ["documents:read", "documents:write"];

pub const AUTHORIZATION_SERVER_METADATA_PATH: &str = "/.well-known/oauth-authorization-server";
pub const AUTHORIZE_PATH: &str = "/oauth/authorize";
pub const TOKEN_PATH: &str = "/oauth/token";
pub const REGISTER_PATH: &str = "/oauth/register";
pub const REVOKE_PATH: &str = "/oauth/revoke";

pub const INTERNAL_CONNECTION_STATE_PATH: &str = "/internal/v1/mcp/connections/{connection_id}";
pub const INTERNAL_SYNC_CREDENTIAL_PATH: &str = "/internal/v1/mcp/sync-credential";
/// Take or renew a document claim (TECHNICAL.final §5.7).
pub const INTERNAL_CLAIMS_PATH: &str = "/internal/v1/mcp/claims";
/// Release a claim. The segment is the claim's handle, which the internal
/// listener never logs.
pub const INTERNAL_CLAIM_PATH: &str = "/internal/v1/mcp/claims/{claim_id}";
/// Every live claim in one workspace, with the call's handles marked.
pub const INTERNAL_CLAIMS_CHECK_PATH: &str = "/internal/v1/mcp/claims/check";
/// A claim handle, in bytes: what any presented handle is bounded by. The
/// API mints 43 (32 random bytes, unpadded base64url).
pub const MAX_CLAIM_HANDLE_BYTES: usize = 128;
/// The header that carries `KORU_MCP_INTERNAL_TOKEN` on every internal call.
/// The person's MCP access token rides `Authorization: Bearer` beside it.
pub const INTERNAL_TOKEN_HEADER: &str = "x-koru-internal-token";

/// The website consent page, after the locale segment:
/// `https://koru.do/<locale>/account/connections/authorize?request=…`.
pub const CONSENT_PAGE_PATH: &str = "/account/connections/authorize";

/// One route outside the account catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolRoute {
    pub method: Method,
    pub path: &'static str,
    pub summary: &'static str,
}

/// The OAuth 2.1 authorization server's public routes, on `us.api.koru.do`.
pub const OAUTH_ROUTES: &[ProtocolRoute] = &[
    ProtocolRoute {
        method: Method::Get,
        path: AUTHORIZATION_SERVER_METADATA_PATH,
        summary: "Authorization server metadata (RFC 8414)",
    },
    ProtocolRoute {
        method: Method::Get,
        path: AUTHORIZE_PATH,
        summary: "Validate an authorization request, then redirect to consent on koru.do",
    },
    ProtocolRoute {
        method: Method::Post,
        path: TOKEN_PATH,
        summary: "Exchange a code (PKCE S256) or rotate a refresh token",
    },
    ProtocolRoute {
        method: Method::Post,
        path: REGISTER_PATH,
        summary: "Dynamic client registration (RFC 7591), public clients only",
    },
    ProtocolRoute {
        method: Method::Post,
        path: REVOKE_PATH,
        summary: "Revoke a refresh token's family (RFC 7009)",
    },
];

/// The routes the MCP host calls on the API's internal listener, with
/// `KORU_MCP_INTERNAL_TOKEN` **and** the person's MCP access token.
pub const INTERNAL_ROUTES: &[ProtocolRoute] = &[
    ProtocolRoute {
        method: Method::Get,
        path: INTERNAL_CONNECTION_STATE_PATH,
        summary: "The connection's live state and resolved Workspaces",
    },
    ProtocolRoute {
        method: Method::Post,
        path: INTERNAL_SYNC_CREDENTIAL_PATH,
        summary: "An app connection sync credential for one replica",
    },
    ProtocolRoute {
        method: Method::Post,
        path: INTERNAL_CLAIMS_PATH,
        summary: "Take or renew a document claim under the claim lock",
    },
    ProtocolRoute {
        method: Method::Delete,
        path: INTERNAL_CLAIM_PATH,
        summary: "Release a document claim; idempotent",
    },
    ProtocolRoute {
        method: Method::Post,
        path: INTERNAL_CLAIMS_CHECK_PATH,
        summary: "Every live claim in a workspace, the presented handles marked",
    },
];

// --- Shared values -----------------------------------------------------------

/// Read, or Read & edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AppConnectionAccessV1 {
    Read,
    ReadWrite,
}

/// The person's choice for one Profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProfileAccessV1 {
    Allowed,
    NotAllowed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppConnectionProfileV1 {
    pub profile_id: ProfileId,
    pub profile_access: ProfileAccessV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AppConnectionRevokedReasonV1 {
    Person,
    RefreshReuse,
    AccountDeleted,
}

/// Who the assistant is, as far as Koru can tell. A metadata-document client
/// is `verified` and names its `domain`; a dynamically registered client is
/// unverified and has no domain. The name is self-declared either way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OAuthClientSummaryV1 {
    pub client_id: String,
    pub name: String,
    pub verified: bool,
    pub domain: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloudSubscriptionStateV1 {
    Active,
    None,
}

// --- Consent (web session) --------------------------------------------------

/// `GET /v1/oauth/consent?request=`: the one query parameter.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OAuthConsentQueryV1 {
    /// The signed pending request, from the consent cookie.
    pub request: String,
}

impl fmt::Debug for OAuthConsentQueryV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthConsentQueryV1(<request>)")
    }
}

/// `GET /v1/oauth/consent?request=`: what the consent page shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OAuthConsentViewV1 {
    pub version: Version1,
    pub client: OAuthClientSummaryV1,
    /// What the assistant asked for. Read & edit is preselected only when it
    /// asked for it.
    pub requested_access: AppConnectionAccessV1,
    /// Case (b) of C15 when `none`.
    pub subscription: CloudSubscriptionStateV1,
    /// Every active Profile. Only the Cloud-syncing ones are offered; none
    /// syncing is case (c).
    pub profiles: Vec<OAuthConsentProfileV1>,
}

/// One active Profile of the account, as the consent view and the
/// connections list carry it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OAuthConsentProfileV1 {
    pub profile_id: ProfileId,
    /// The account's discovery snapshot; absent shows "Unnamed Profile".
    pub name_snapshot: Option<String>,
    pub cloud_syncing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OAuthConsentDecisionV1 {
    Approve,
    Deny,
}

/// `POST /v1/oauth/consent`. Approving needs `name` and `access`, and a
/// decision for every Cloud-syncing Profile. Denying needs neither.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OAuthConsentDecisionRequestV1 {
    pub version: Version1,
    /// The signed pending request, from the consent cookie.
    pub request: String,
    pub decision: OAuthConsentDecisionV1,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub access: Option<AppConnectionAccessV1>,
    #[serde(default)]
    pub allow_profiles_enabled_later: bool,
    #[serde(default)]
    pub profiles: Vec<AppConnectionProfileV1>,
}

impl fmt::Debug for OAuthConsentDecisionRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthConsentDecisionRequestV1")
            .field("decision", &self.decision)
            .field("access", &self.access)
            .field("profiles", &self.profiles.len())
            .finish_non_exhaustive()
    }
}

/// Where the browser goes next: the client's redirect URI with `code`,
/// `state` and `iss`, or with `error=access_denied`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OAuthConsentDecisionResponseV1 {
    pub version: Version1,
    pub redirect_to: String,
}

impl fmt::Debug for OAuthConsentDecisionResponseV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthConsentDecisionResponseV1(<redirect>)")
    }
}

// --- Connections (account session) ------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppConnectionSummaryV1 {
    pub version: Version1,
    pub connection_id: AppConnectionId,
    pub name: String,
    pub client: OAuthClientSummaryV1,
    pub access: AppConnectionAccessV1,
    pub allow_profiles_enabled_later: bool,
    pub profiles: Vec<AppConnectionProfileV1>,
    pub created_at_millis: i64,
    pub last_used_at_millis: i64,
    pub revoked_at_millis: Option<i64>,
    pub revoked_reason: Option<AppConnectionRevokedReasonV1>,
}

/// `GET /v1/account/connections`: every connection, live and revoked, and the
/// account's active Profiles, so a list can name each connection's Profiles
/// and offer the ones it has no decision for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppConnectionListResponseV1 {
    pub version: Version1,
    pub connections: Vec<AppConnectionSummaryV1>,
    /// Every active Profile of the account, read as consent reads them.
    pub profiles: Vec<OAuthConsentProfileV1>,
}

/// `PATCH /v1/account/connections/{connection_id}`. At least one change.
/// Widening to Read & edit reaches the assistant when it next connects; the
/// tokens it holds keep the ceiling it was approved with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateAppConnectionRequestV1 {
    pub version: Version1,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub access: Option<AppConnectionAccessV1>,
    #[serde(default)]
    pub allow_profiles_enabled_later: Option<bool>,
    #[serde(default)]
    pub profiles: Vec<AppConnectionProfileV1>,
}

// --- OAuth protocol documents (RFC-shaped, unversioned) ---------------------

/// RFC 8414 metadata, as `/.well-known/oauth-authorization-server` serves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthorizationServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: String,
    pub revocation_endpoint: String,
    pub response_types_supported: Vec<String>,
    pub grant_types_supported: Vec<String>,
    pub code_challenge_methods_supported: Vec<String>,
    pub token_endpoint_auth_methods_supported: Vec<String>,
    pub revocation_endpoint_auth_methods_supported: Vec<String>,
    pub scopes_supported: Vec<String>,
    pub client_id_metadata_document_supported: bool,
    pub authorization_response_iss_parameter_supported: bool,
}

/// RFC 7591's request, as far as Koru reads it. Unknown members are ignored,
/// as the RFC requires; a confidential auth method or a grant Koru does not
/// offer is refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientRegistrationRequest {
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
    #[serde(default)]
    pub grant_types: Option<Vec<String>>,
    #[serde(default)]
    pub response_types: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientRegistrationResponse {
    pub client_id: String,
    pub client_id_issued_at: i64,
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<String>,
    pub response_types: Vec<String>,
    pub token_endpoint_auth_method: String,
}

/// A Client ID Metadata Document, as far as Koru reads it. Other members are
/// ignored; a logo is never shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientMetadataDocument {
    pub client_id: String,
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
}

/// RFC 6749 §5.1.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub refresh_token: String,
    pub scope: String,
}

impl fmt::Debug for OAuthTokenResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthTokenResponse")
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// RFC 6749 §5.2 and RFC 7591 §3.2.2.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OAuthErrorResponse {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_description: Option<String>,
}

// --- Internal routes ---------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppConnectionLifecycleV1 {
    Live,
    Revoked,
}

/// `GET /internal/v1/mcp/connections/{connection_id}`, read now. The host
/// keeps it at most 60 seconds (admission's `valid_until`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConnectionStateV1 {
    pub version: Version1,
    pub connection_id: AppConnectionId,
    pub koru_identity_id: KoruIdentityId,
    pub client_id: String,
    pub state: AppConnectionLifecycleV1,
    pub access: AppConnectionAccessV1,
    pub subscription: CloudSubscriptionStateV1,
    /// The entitlement's end, when it has one.
    pub active_until_millis: Option<i64>,
    pub allow_profiles_enabled_later: bool,
    pub profiles: Vec<AppConnectionProfileV1>,
    /// Empty when revoked or not entitled.
    pub workspaces: Vec<AppConnectionWorkspaceV1>,
    pub read_at_millis: i64,
    /// The connection's name now, 1 to 64 characters, which every commit the
    /// host makes under it carries in its `actor/1` attribution
    /// (`contracts/connector-attribution-v1.md`; Cloud MCP Q2.3-1).
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConnectionWorkspaceV1 {
    pub workspace_id: WorkspaceId,
    pub profile_id: ProfileId,
    pub workspace_incarnation_id: Uuid,
    pub authorization_version: i64,
    pub membership_epoch: i64,
    pub cloud_access_epoch: i64,
    /// The manifest's Store document id in the Cloud catalog, once Cloud holds
    /// the manifest. The replica's carrier scope and its Store both need it;
    /// absent, the Workspace's names are `unavailable` (Cloud MCP Q1.6-1).
    #[serde(default)]
    pub manifest_document_id: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppConnectionStageV1 {
    Manifest,
    Full,
}

/// `POST /internal/v1/mcp/sync-credential`. The answer is a
/// [`crate::SyncTokenResponseV1`] carrying an app connection credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConnectionSyncCredentialRequestV1 {
    pub version: Version1,
    pub workspace_id: WorkspaceId,
    pub profile_id: ProfileId,
    /// The replica's endpoint key, in its storage spelling.
    pub endpoint_id: String,
    pub stage: AppConnectionStageV1,
}

// --- Document claims (internal) ---------------------------------------------

/// `POST /internal/v1/mcp/claims`: `claim_document`'s input as the host
/// forwards it. With `claim_id`, the call **renews** that claim; without, the
/// API mints a new handle. `purpose` is 1–120 characters, `agent_label`
/// 1–64, both self-declared; `duration_seconds` is 60–900, default 300.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentClaimRequestV1 {
    pub version: Version1,
    pub workspace_id: WorkspaceId,
    pub document_id: DocumentId,
    #[serde(default)]
    pub claim_id: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub agent_label: Option<String>,
    #[serde(default)]
    pub duration_seconds: Option<u32>,
}

impl fmt::Debug for DocumentClaimRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DocumentClaimRequestV1")
            .field("workspace_id", &self.workspace_id)
            .field("document_id", &self.document_id)
            .field("renew", &self.claim_id.is_some())
            .field("duration_seconds", &self.duration_seconds)
            .finish_non_exhaustive()
    }
}

/// Who holds a claim: the connection's name, its client (the verified domain,
/// or the client's own name when unverified), whether it is verified, and the
/// self-declared label.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentClaimHolderV1 {
    pub connection_name: String,
    pub client: String,
    pub verified: bool,
    pub agent_label: Option<String>,
}

/// One live claim, as any agent of the account may see it. Never the handle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentClaimV1 {
    pub workspace_id: WorkspaceId,
    pub document_id: DocumentId,
    pub holder: DocumentClaimHolderV1,
    pub purpose: Option<String>,
    pub expires_at_millis: i64,
}

/// `POST /internal/v1/mcp/claims`, 200: the claim taken or renewed. Renewal
/// keeps the handle.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentClaimGrantV1 {
    pub version: Version1,
    pub claim_id: String,
    pub renewed: bool,
    pub claim: DocumentClaimV1,
}

impl fmt::Debug for DocumentClaimGrantV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DocumentClaimGrantV1")
            .field("renewed", &self.renewed)
            .field("claim", &self.claim)
            .finish_non_exhaustive()
    }
}

/// `POST /internal/v1/mcp/claims`, 409 `document_claimed`: who holds the
/// document, what for and until when, judged by the database clock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentClaimedV1 {
    pub code: String,
    pub claim: DocumentClaimV1,
    pub retry_after_seconds: i64,
}

/// `DELETE /internal/v1/mcp/claims/{claim_id}`, 200. `released` is false
/// when the handle names no claim of this connection, or one already
/// released; the tool answers the same either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentClaimReleaseV1 {
    pub version: Version1,
    pub released: bool,
}

/// `POST /internal/v1/mcp/claims/check`: the workspace, and the handles the
/// call presents (at most 16).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentClaimCheckRequestV1 {
    pub version: Version1,
    pub workspace_id: WorkspaceId,
    #[serde(default)]
    pub claim_ids: Vec<String>,
}

impl fmt::Debug for DocumentClaimCheckRequestV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DocumentClaimCheckRequestV1")
            .field("workspace_id", &self.workspace_id)
            .field("claim_ids", &self.claim_ids.len())
            .finish()
    }
}

/// One live claim of the checked workspace. `presented` is true when the
/// call presented this claim's handle and the claim is its connection's: an
/// edit of the document may go ahead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckedDocumentClaimV1 {
    pub claim: DocumentClaimV1,
    pub presented: bool,
}

/// `POST /internal/v1/mcp/claims/check`, 200: every live claim in the
/// workspace, read now. A document absent from `claims` is unclaimed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentClaimCheckV1 {
    pub version: Version1,
    pub read_at_millis: i64,
    pub claims: Vec<CheckedDocumentClaimV1>,
}

impl From<koru_account_core::workspace_authorization::AppConnectionAccess>
    for AppConnectionAccessV1
{
    fn from(value: koru_account_core::workspace_authorization::AppConnectionAccess) -> Self {
        use koru_account_core::workspace_authorization::AppConnectionAccess as Core;
        match value {
            Core::Read => Self::Read,
            Core::ReadWrite => Self::ReadWrite,
        }
    }
}

impl From<AppConnectionAccessV1>
    for koru_account_core::workspace_authorization::AppConnectionAccess
{
    fn from(value: AppConnectionAccessV1) -> Self {
        match value {
            AppConnectionAccessV1::Read => Self::Read,
            AppConnectionAccessV1::ReadWrite => Self::ReadWrite,
        }
    }
}

impl From<koru_account_core::app_connections::ProfileAccess> for ProfileAccessV1 {
    fn from(value: koru_account_core::app_connections::ProfileAccess) -> Self {
        use koru_account_core::app_connections::ProfileAccess as Core;
        match value {
            Core::Allowed => Self::Allowed,
            Core::NotAllowed => Self::NotAllowed,
        }
    }
}

impl From<ProfileAccessV1> for koru_account_core::app_connections::ProfileAccess {
    fn from(value: ProfileAccessV1) -> Self {
        match value {
            ProfileAccessV1::Allowed => Self::Allowed,
            ProfileAccessV1::NotAllowed => Self::NotAllowed,
        }
    }
}

impl From<koru_account_core::app_connections::RevokedReason> for AppConnectionRevokedReasonV1 {
    fn from(value: koru_account_core::app_connections::RevokedReason) -> Self {
        use koru_account_core::app_connections::RevokedReason as Core;
        match value {
            Core::Person => Self::Person,
            Core::RefreshReuse => Self::RefreshReuse,
            Core::AccountDeleted => Self::AccountDeleted,
        }
    }
}
