// SPDX-License-Identifier: MPL-2.0

//! Document claims (Cloud MCP TECHNICAL.final C13, §5.7): an exclusive,
//! server-held, expiring lease one hosted agent takes on one document, so
//! other hosted agents' edits wait their turn. The persistence port the
//! internal claim routes run over; the schema and the lock are
//! `docs/cloud-platform/contracts/app-connections-v1.md`.
//!
//! A claim's handle is a bearer secret minted by the API. Only its SHA-256
//! crosses this port, and that digest is also the reference a connection's
//! upload carries, which the Cloud worker matches at the publish commit.
//!
//! **A claim is live** while it is unexpired by the database clock and its
//! connection is not revoked: the same rule the worker enforces. Acquiring
//! also ends a claim whose connection no longer covers the workspace (C13,
//! "loses the workspace").

use koru_store_core::{DocumentId, WorkspaceId};

use super::AppConnectionError;
use crate::{AppConnectionId, KoruIdentityId, PortFuture};

/// Live claims one connection may hold (`claim_limit_reached`).
pub const MAX_LIVE_CLAIMS: usize = 16;
/// A claim's duration, in seconds: 60–900, default 300.
pub const MIN_CLAIM_SECONDS: u32 = 60;
pub const MAX_CLAIM_SECONDS: u32 = 900;
pub const DEFAULT_CLAIM_SECONDS: u32 = 300;
/// The self-declared texts, in characters.
pub const MAX_PURPOSE_CHARS: usize = 120;
pub const MAX_AGENT_LABEL_CHARS: usize = 64;

/// A requested duration in milliseconds, or `None` outside 60–900 seconds.
pub fn claim_duration_millis(seconds: Option<u32>) -> Option<i64> {
    let seconds = seconds.unwrap_or(DEFAULT_CLAIM_SECONDS);
    (MIN_CLAIM_SECONDS..=MAX_CLAIM_SECONDS)
        .contains(&seconds)
        .then(|| i64::from(seconds) * 1_000)
}

/// A self-declared text (`purpose`, `agent_label`): trimmed, 1 to `max`
/// characters, no control characters. `None` when it fails any of these.
pub fn claim_text(value: &str, max: usize) -> Option<String> {
    let trimmed = value.trim();
    let count = trimmed.chars().count();
    ((1..=max).contains(&count) && !trimmed.chars().any(char::is_control))
        .then(|| trimmed.to_owned())
}

/// Whole seconds until `expires_at_millis`, rounded up, never below one:
/// the `retry_after_seconds` of a `document_claimed` refusal.
pub fn retry_after_seconds(expires_at_millis: i64, now_millis: i64) -> i64 {
    let remaining = expires_at_millis.saturating_sub(now_millis);
    remaining.saturating_add(999).div_euclid(1_000).max(1)
}

/// Who holds a claim, as other agents are told: the holding connection's
/// name and client, and the self-declared agent label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimHolder {
    pub connection_name: String,
    pub client_name: String,
    /// The metadata document's host when the client is verified.
    pub verified_host: Option<String>,
    pub agent_label: Option<String>,
}

/// One live claim, without its handle or its digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentClaim {
    pub workspace_id: WorkspaceId,
    pub document_id: DocumentId,
    pub holder: ClaimHolder,
    pub purpose: Option<String>,
    pub acquired_at_millis: i64,
    pub expires_at_millis: i64,
}

/// Take a claim, or renew one. The caller has checked the token; the adapter
/// checks the connection itself, in the claim's own transaction.
#[derive(Clone, PartialEq, Eq)]
pub struct ClaimRequest {
    /// The token's connection and subject: the connection must be the
    /// subject's, live, Read & edit, and still cover `workspace_id`.
    pub connection_id: AppConnectionId,
    pub koru_identity_id: KoruIdentityId,
    pub workspace_id: WorkspaceId,
    pub document_id: DocumentId,
    /// SHA-256 of the handle: a fresh one to take the claim, or the held one
    /// to renew it.
    pub claim_hash: [u8; 32],
    pub renew: bool,
    /// On renewal, an absent text keeps the stored one.
    pub purpose: Option<String>,
    pub agent_label: Option<String>,
    pub duration_millis: i64,
}

impl std::fmt::Debug for ClaimRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimRequest")
            .field("connection_id", &self.connection_id)
            .field("workspace_id", &self.workspace_id)
            .field("document_id", &self.document_id)
            .field("renew", &self.renew)
            .field("duration_millis", &self.duration_millis)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// Taken or renewed. `ended_stale` is true when taking it ended another
    /// claim that had expired, or whose connection was revoked or no longer
    /// covers the workspace.
    Granted {
        claim: DocumentClaim,
        renewed: bool,
        ended_stale: bool,
    },
    /// Another live claim holds the document: another connection's, or this
    /// connection's under another handle. `read_at_millis` is the database
    /// clock the claim was judged by.
    Claimed {
        claim: DocumentClaim,
        read_at_millis: i64,
    },
    /// The connection already holds [`MAX_LIVE_CLAIMS`] live claims.
    LimitReached,
    /// Renewing a handle this connection does not hold for this document.
    NotFound,
    /// Renewing a handle whose claim has expired. The row is deleted.
    Expired,
    /// The workspace is outside the connection's resolved set.
    WorkspaceNotAuthorized,
    /// The connection is Read only.
    ReadOnly,
    /// The connection is revoked, or is not the subject's.
    Revoked,
}

/// A claim released by its own connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleasedClaim {
    pub workspace_id: WorkspaceId,
    pub document_id: DocumentId,
    /// False when the row had already expired.
    pub was_live: bool,
}

/// Every live claim in one workspace, for the host's early check and for
/// reads. `presented` marks the claims the call holds a handle for.
#[derive(Clone, PartialEq, Eq)]
pub struct ClaimCheckRequest {
    pub connection_id: AppConnectionId,
    pub koru_identity_id: KoruIdentityId,
    pub workspace_id: WorkspaceId,
    /// SHA-256 of each handle the call presents.
    pub presented: Vec<[u8; 32]>,
}

impl std::fmt::Debug for ClaimCheckRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimCheckRequest")
            .field("connection_id", &self.connection_id)
            .field("workspace_id", &self.workspace_id)
            .field("presented", &self.presented.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedClaim {
    pub claim: DocumentClaim,
    /// A presented handle is this claim's, and the claim is this
    /// connection's.
    pub presented: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimCheck {
    Claims {
        read_at_millis: i64,
        claims: Vec<CheckedClaim>,
    },
    WorkspaceNotAuthorized,
    Revoked,
}

pub trait DocumentClaimRepository: Send + Sync {
    /// Take or renew, in one transaction under the claim lock, judged by the
    /// database clock.
    fn claim<'a>(
        &'a self,
        request: ClaimRequest,
    ) -> PortFuture<'a, Result<ClaimOutcome, AppConnectionError>>;

    /// Release the claim `claim_hash` names, under the claim lock, when it is
    /// `connection_id`'s. Anything else changes nothing and answers `None`.
    fn release<'a>(
        &'a self,
        connection_id: AppConnectionId,
        claim_hash: [u8; 32],
    ) -> PortFuture<'a, Result<Option<ReleasedClaim>, AppConnectionError>>;

    /// Every live claim in the workspace, read now. Takes no lock: the worker
    /// decides at the publish commit, and this is the early answer.
    fn check<'a>(
        &'a self,
        request: ClaimCheckRequest,
    ) -> PortFuture<'a, Result<ClaimCheck, AppConnectionError>>;
}

#[cfg(test)]
mod tests;
