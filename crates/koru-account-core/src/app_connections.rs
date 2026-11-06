// SPDX-License-Identifier: MPL-2.0

//! App connections: an OAuth client's approved, per-account connection (an AI
//! assistant reaching Koru's hosted MCP server, first). The persistence port
//! the authorization server and the connection routes run over, and the one
//! rule that resolves a connection's Workspaces (Cloud MCP TECHNICAL.final
//! §6.1; the schema is `docs/cloud-platform/contracts/app-connections-v1.md`).
//!
//! No secret crosses this port: codes and refresh tokens are SHA-256 digests,
//! minted and compared by the caller.

use koru_store_core::{ProfileId, WorkspaceId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::sync_issuance::{SyncIssuanceAuthority, SyncIssuanceError};
use crate::workspace_authorization::AppConnectionAccess;
use crate::{AppConnectionId, KoruIdentityId, PortFuture};

/// A connection's and a client's name: 1–64 characters, trimmed.
pub const MAX_NAME_CHARS: usize = 64;
/// Redirect URIs per client.
pub const MAX_REDIRECT_URIS: usize = 16;
/// A client id or a redirect URI, in bytes.
pub const MAX_URI_BYTES: usize = 2048;
/// An authorization code lives one minute.
pub const AUTHORIZATION_CODE_MILLIS: i64 = 60_000;
/// A fetched client metadata document is trusted for a day.
pub const METADATA_CACHE_MILLIS: i64 = 24 * 60 * 60 * 1_000;
/// `last_used_at` is written at most this often.
pub const LAST_USED_RESOLUTION_MILLIS: i64 = 60 * 60 * 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppConnectionError {
    /// The row named does not exist for this caller.
    NotFound,
    /// A decision names a Profile the account does not hold, or omits one of
    /// its Cloud-syncing Profiles at approval.
    ProfilesChanged,
    /// The client is not registered.
    ClientUnknown,
    Conflict,
    Unavailable,
    /// Stored state violated an invariant.
    Integrity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthClientRegistration {
    MetadataDocument,
    Dynamic,
}

/// One OAuth client. A metadata-document client's id is the https URL of its
/// document and `verified_host` is that URL's host; a dynamic client's id is
/// `dcr:` and a v4 UUID, never verified. No logo, ever.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthClientRecord {
    pub client_id: String,
    pub registration: OAuthClientRegistration,
    pub client_name: String,
    pub verified_host: Option<String>,
    pub redirect_uris: Vec<String>,
    pub metadata_fetched_at_millis: Option<i64>,
    pub created_at_millis: i64,
    pub last_used_at_millis: i64,
}

/// What the person decided for one Profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileAccess {
    Allowed,
    NotAllowed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileDecision {
    pub profile_id: ProfileId,
    pub access: ProfileAccess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileDecisionRecord {
    pub profile_id: ProfileId,
    pub access: ProfileAccess,
    pub decided_at_millis: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevokedReason {
    Person,
    RefreshReuse,
    AccountDeleted,
}

/// One connection as stored, with its client and its Profile decisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConnectionRecord {
    pub connection_id: AppConnectionId,
    pub koru_identity_id: KoruIdentityId,
    pub client: OAuthClientRecord,
    pub name: String,
    pub access: AppConnectionAccess,
    pub allow_profiles_enabled_later: bool,
    pub profiles: Vec<ProfileDecisionRecord>,
    pub created_at_millis: i64,
    pub last_used_at_millis: i64,
    pub revoked_at_millis: Option<i64>,
    pub revoked_reason: Option<RevokedReason>,
}

impl AppConnectionRecord {
    pub fn is_live(&self) -> bool {
        self.revoked_at_millis.is_none()
    }
}

/// A Workspace the account may reach now: Cloud-enabled, in the US, in an
/// active Profile of the account, under an active membership that reads and
/// syncs. The input to [`resolve_workspaces`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudWorkspaceCandidate {
    pub profile_id: ProfileId,
    pub workspace_id: WorkspaceId,
    pub workspace_incarnation_id: Uuid,
    pub authorization_version: i64,
    pub membership_epoch: i64,
    pub cloud_access_epoch: i64,
}

/// **The resolved Workspace set** (TECHNICAL.final §6.1): Workspaces
/// Cloud-enabled now, in Profiles where the connection has `allowed`, **or**,
/// when `allow_profiles_enabled_later` is set, in Cloud Profiles that have no
/// row. A Profile decided `not_allowed` never comes back through the flag.
///
/// `candidates` is already the account's current authorization (every
/// candidate is Cloud-enabled under a live membership), so the intersection
/// with current authorization is the candidates themselves. A Profile is a
/// Cloud Profile exactly when it has a candidate. The order of `candidates`
/// is kept.
pub fn resolve_workspaces(
    candidates: Vec<CloudWorkspaceCandidate>,
    decisions: &[ProfileDecision],
    allow_profiles_enabled_later: bool,
) -> Vec<CloudWorkspaceCandidate> {
    candidates
        .into_iter()
        .filter(|candidate| {
            match decisions
                .iter()
                .find(|decision| decision.profile_id == candidate.profile_id)
            {
                Some(decision) => decision.access == ProfileAccess::Allowed,
                None => allow_profiles_enabled_later,
            }
        })
        .collect()
}

/// A Profile as the consent page lists it, before any replica exists: the
/// account's `name_snapshot` (C17) and whether it syncs with Cloud now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsentProfile {
    pub profile_id: ProfileId,
    pub name_snapshot: Option<String>,
    pub cloud_syncing: bool,
}

/// A fresh authorization code, stored as its digest with the ceiling the
/// person approved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewAuthorizationCode {
    pub code_hash: [u8; 32],
    pub redirect_uri: String,
    pub code_challenge: String,
    pub resource: String,
    pub ceiling: AppConnectionAccess,
    pub expires_at_millis: i64,
}

/// Everything one approval writes, in one transaction: the connection, a row
/// for every Profile decided, and the code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub connection_id: AppConnectionId,
    pub koru_identity_id: KoruIdentityId,
    pub client_id: String,
    pub name: String,
    pub access: AppConnectionAccess,
    pub allow_profiles_enabled_later: bool,
    pub decisions: Vec<ProfileDecision>,
    pub code: NewAuthorizationCode,
    pub now_millis: i64,
}

/// An unexpired, unconsumed code, consumed by the read that returned it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizationCodeGrant {
    pub connection: AppConnectionRecord,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub resource: String,
    pub ceiling: AppConnectionAccess,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeRedemption {
    Redeemed(Box<AuthorizationCodeGrant>),
    /// A second use: the connection's refresh tokens were revoked in the same
    /// transaction (RFC 6749 §4.1.2).
    Reused,
    /// Unknown or expired.
    Invalid,
}

/// A refresh token to store. A family's first token has no parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRefreshToken {
    pub token_hash: [u8; 32],
    pub connection_id: AppConnectionId,
    pub family_id: Uuid,
    pub ceiling: AppConnectionAccess,
    pub issued_at_millis: i64,
    pub expires_at_millis: i64,
}

/// Rotate `presented_hash` into `child_hash`, in one transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefreshRotationRequest {
    pub presented_hash: [u8; 32],
    pub child_hash: [u8; 32],
    pub client_id: String,
    pub now_millis: i64,
    /// Without a family boundary; the adapter caps it at the family's
    /// absolute lifetime, read in the same transaction.
    pub idle_expires_at_millis: i64,
    pub absolute_lifetime_millis: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefreshGrant {
    pub connection: AppConnectionRecord,
    /// The family's ceiling, copied unchanged onto the child.
    pub ceiling: AppConnectionAccess,
    pub family_id: Uuid,
    pub expires_at_millis: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshRotation {
    Rotated(Box<RefreshGrant>),
    /// A consumed token presented again: the connection was revoked
    /// (`refresh_reuse`) with every refresh token it has.
    Reused,
    /// Unknown, expired, revoked, another client's, or its connection is
    /// revoked. Nothing changed.
    Invalid,
}

/// A person's edit of one connection. Absent fields are kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionUpdate {
    pub koru_identity_id: KoruIdentityId,
    pub connection_id: AppConnectionId,
    pub name: Option<String>,
    pub access: Option<AppConnectionAccess>,
    pub allow_profiles_enabled_later: Option<bool>,
    /// Writes a row for every Profile it names; the others keep theirs.
    pub decisions: Vec<ProfileDecision>,
    pub now_millis: i64,
}

/// A connection-state read: the stored connection and its resolved
/// Workspaces, computed now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConnectionState {
    pub connection: AppConnectionRecord,
    pub workspaces: Vec<CloudWorkspaceCandidate>,
}

pub trait AppConnectionRepository: Send + Sync {
    fn find_client<'a>(
        &'a self,
        client_id: &'a str,
    ) -> PortFuture<'a, Result<Option<OAuthClientRecord>, AppConnectionError>>;

    /// Insert a dynamic client, or insert or refresh a metadata-document
    /// client (its name, redirect URIs, host and fetch time; never its id).
    fn save_client<'a>(
        &'a self,
        client: OAuthClientRecord,
    ) -> PortFuture<'a, Result<(), AppConnectionError>>;

    /// Every active Profile of the account, for the consent page.
    fn consent_profiles<'a>(
        &'a self,
        koru_identity_id: KoruIdentityId,
    ) -> PortFuture<'a, Result<Vec<ConsentProfile>, AppConnectionError>>;

    /// One approval. `ProfilesChanged` when a decision names a Profile the
    /// account does not hold or a Cloud-syncing Profile has no decision.
    fn approve<'a>(&'a self, approval: Approval) -> PortFuture<'a, Result<(), AppConnectionError>>;

    fn redeem_code<'a>(
        &'a self,
        code_hash: [u8; 32],
        now_millis: i64,
    ) -> PortFuture<'a, Result<CodeRedemption, AppConnectionError>>;

    /// A family's first refresh token, after a code was redeemed.
    fn start_refresh_family<'a>(
        &'a self,
        token: NewRefreshToken,
    ) -> PortFuture<'a, Result<(), AppConnectionError>>;

    /// Whose connection a live refresh token belongs to, without consuming
    /// it, so the entitlement can be checked before rotation.
    fn refresh_owner<'a>(
        &'a self,
        token_hash: [u8; 32],
        now_millis: i64,
    ) -> PortFuture<'a, Result<Option<KoruIdentityId>, AppConnectionError>>;

    fn rotate_refresh<'a>(
        &'a self,
        request: RefreshRotationRequest,
    ) -> PortFuture<'a, Result<RefreshRotation, AppConnectionError>>;

    /// RFC 7009: revoke the family `token_hash` belongs to, when the token is
    /// the client's. Unknown tokens change nothing.
    fn revoke_refresh_family<'a>(
        &'a self,
        token_hash: [u8; 32],
        client_id: &'a str,
        now_millis: i64,
    ) -> PortFuture<'a, Result<(), AppConnectionError>>;

    /// Every connection of the account, live and revoked, newest first.
    fn list_connections<'a>(
        &'a self,
        koru_identity_id: KoruIdentityId,
    ) -> PortFuture<'a, Result<Vec<AppConnectionRecord>, AppConnectionError>>;

    fn update_connection<'a>(
        &'a self,
        update: ConnectionUpdate,
    ) -> PortFuture<'a, Result<AppConnectionRecord, AppConnectionError>>;

    /// The person revokes: the connection is retired with reason `person`
    /// and every refresh token it has is revoked. Revoking a revoked
    /// connection returns it unchanged.
    fn revoke_connection<'a>(
        &'a self,
        koru_identity_id: KoruIdentityId,
        connection_id: AppConnectionId,
        now_millis: i64,
    ) -> PortFuture<'a, Result<AppConnectionRecord, AppConnectionError>>;

    /// The connection and its resolved Workspaces, read now. Also records the
    /// use (at most hourly) when the connection is live.
    fn connection_state<'a>(
        &'a self,
        connection_id: AppConnectionId,
        now_millis: i64,
    ) -> PortFuture<'a, Result<Option<AppConnectionState>, AppConnectionError>>;

    /// The dataset authority a connection's sync credential is signed from:
    /// the connection is live, `workspace` is in its resolved set under
    /// `profile`, and both datasets are assigned. The device issuer's
    /// refusals, so the two credentials fail alike.
    fn sync_authority<'a>(
        &'a self,
        connection_id: AppConnectionId,
        profile: ProfileId,
        workspace: WorkspaceId,
        now_millis: i64,
    ) -> PortFuture<'a, Result<SyncIssuanceAuthority, SyncIssuanceError>>;
}

/// The account's Cloud entitlement, for consent case (b), every token
/// issuance, and the connection state's `active_until`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudEntitlement {
    /// Entitled. `None` is no known end (the deployment bills nothing).
    Active { until_millis: Option<i64> },
    /// No Cloud subscription, or it has ended.
    None,
}

impl CloudEntitlement {
    pub fn is_active_at(self, now_millis: i64) -> bool {
        match self {
            Self::Active { until_millis } => until_millis.is_none_or(|until| until > now_millis),
            Self::None => false,
        }
    }
}

pub trait CloudEntitlementPort: Send + Sync {
    fn entitlement<'a>(
        &'a self,
        koru_identity_id: KoruIdentityId,
        now_millis: i64,
    ) -> PortFuture<'a, Result<CloudEntitlement, AppConnectionError>>;
}

#[cfg(test)]
mod tests;
