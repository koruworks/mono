// SPDX-License-Identifier: MPL-2.0

//! **Access re-resolved between admission and execution** (root-catalog
//! `docs/features/root-catalog/TECHNICAL.final.md` §7.4, §11.4; the
//! roadmap's Plan 3.3).
//!
//! A hosted session admits a call with the workspace set its connection
//! resolved to, and that set can change while the session lives: a
//! Profile grant follows the Profile's current placement, so a Workspace
//! moved out of a granted Profile leaves the set, and one moved in joins
//! it; a Workspace grant stays pinned to its UUID. The executor therefore
//! asks an [`AccessResolver`] **before each tool executes** whether the
//! context's access is still current, uses the re-resolved set when it is
//! not, and refuses a stale grant rather than let an old session keep
//! moved-out access.
//!
//! **Nothing about this is a tool input.** No tool gains a `profile_id`, no
//! organization tool is added, and the published catalog is byte-for-byte
//! what it was: a UUID named in a call is checked against the *current*
//! set, and a Workspace that left the grant answers the same
//! `permission_denied` an unselected one always did.
//!
//! **Projection lag is pending, never a fallback** (§7.4). A Workspace the
//! resolver cannot decide yet — the service's projection has not caught up
//! with an admitted move — is listed in [`ResolvedAccess::pending`]; a call
//! naming it answers [`authorization_unavailable`](pending) with a retry
//! hint, the admission refusal `contracts/hosted-tools-v1.md` §2 already
//! defines as "nothing was saved and a retry is safe once the cause is
//! gone". It is neither served from another Workspace nor denied.
//!
//! Local contexts have [`WorkspaceAccess::All`](crate::WorkspaceAccess::All)
//! and no resolver: nothing here runs for the desktop bridge or Koru AI.

use std::sync::Arc;

use serde_json::json;

use crate::{StoreWorkspaceId, ToolError, WorkspaceAccess};

/// The code a call that cannot be authorized *yet* answers: the admission
/// refusal of `contracts/hosted-tools-v1.md` §2, reused — a retry is safe.
pub const AUTHORIZATION_UNAVAILABLE: &str = "authorization_unavailable";
/// The code a call whose connection no longer authorizes it answers.
pub const AUTHORIZATION_REVOKED: &str = "authorization_revoked";

/// What a session's access was resolved under. **Opaque to this crate**:
/// the host fills the three counters from its authority — the service's
/// projection pass or placement key for `placement_revision`, the account's
/// membership epoch and authorization version — and the resolver compares
/// them. The executor never interprets them; it carries them so a resolver
/// can answer [`AccessResolution::Current`] cheaply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AccessRevision {
    pub placement_revision: u64,
    pub membership_epoch: u64,
    pub authorization_version: u64,
}

/// A Workspace whose grant the resolver cannot decide yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingWorkspace {
    pub workspace_id: StoreWorkspaceId,
    pub retry_after_seconds: u64,
}

/// A freshly resolved set, with what is still pending and the revision it
/// was resolved under.
#[derive(Clone, Debug)]
pub struct ResolvedAccess {
    pub access: WorkspaceAccess,
    pub pending: Vec<PendingWorkspace>,
    pub revision: AccessRevision,
}

/// What a resolver answers when asked whether a context's access still
/// holds.
#[derive(Clone, Debug)]
pub enum AccessResolution {
    /// The context's access is current; use it as admitted.
    Current,
    /// The set changed (or was never cached): use this one for the call.
    Resolved(ResolvedAccess),
    /// The authority cannot be read right now; nothing is served from a
    /// stale set. The call answers [`AUTHORIZATION_UNAVAILABLE`].
    Unavailable { retry_after_seconds: u64 },
    /// The connection is revoked or no longer entitled. The call answers
    /// [`AUTHORIZATION_REVOKED`].
    Revoked,
}

/// The hosted host's seam. It is asked once per tool call that needs a
/// content scope — including every `list_*` and `search_*` — with the
/// revision the context was admitted under, and answers from its cache of
/// connection-state reads (at most 60 seconds old) or from a fresh read.
/// A device never installs one.
pub trait AccessResolver: Send + Sync {
    fn resolve(&self, admitted: Option<&AccessRevision>) -> AccessResolution;
}

/// The optional resolver on a [`RequestContext`](crate::RequestContext).
/// `Debug` says only whether one is set.
#[derive(Clone, Default)]
pub struct AccessResolverSeam(pub(crate) Option<Arc<dyn AccessResolver>>);

impl std::fmt::Debug for AccessResolverSeam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "AccessResolverSeam(set)"
        } else {
            "AccessResolverSeam(none)"
        })
    }
}

/// The access one call runs under after re-resolution: the context's own,
/// or the resolver's fresh set with its pending Workspaces.
pub(crate) struct CurrentAccess<'a> {
    pub access: std::borrow::Cow<'a, WorkspaceAccess>,
    pub pending: Vec<PendingWorkspace>,
}

impl<'a> CurrentAccess<'a> {
    pub(crate) fn admitted(access: &'a WorkspaceAccess) -> Self {
        Self {
            access: std::borrow::Cow::Borrowed(access),
            pending: Vec::new(),
        }
    }

    /// The pending entry for `workspace`, when its grant is undecided.
    pub(crate) fn pending_for(&self, workspace: StoreWorkspaceId) -> Option<PendingWorkspace> {
        self.pending
            .iter()
            .copied()
            .find(|pending| pending.workspace_id == workspace)
    }
}

impl AccessResolverSeam {
    /// Ask the resolver, when there is one, and turn its answer into the
    /// access this call runs under or the refusal it gets.
    pub(crate) fn current<'a>(
        &self,
        tool: &str,
        admitted: &'a WorkspaceAccess,
        revision: Option<&AccessRevision>,
    ) -> Result<CurrentAccess<'a>, ToolError> {
        let Some(resolver) = &self.0 else {
            return Ok(CurrentAccess::admitted(admitted));
        };
        match resolver.resolve(revision) {
            AccessResolution::Current => Ok(CurrentAccess::admitted(admitted)),
            AccessResolution::Resolved(resolved) => Ok(CurrentAccess {
                access: std::borrow::Cow::Owned(resolved.access),
                pending: resolved.pending,
            }),
            AccessResolution::Unavailable {
                retry_after_seconds,
            } => Err(unavailable(tool, retry_after_seconds)),
            AccessResolution::Revoked => Err(revoked(tool)),
        }
    }
}

/// The typed retry for a Workspace whose grant is pending: the projection
/// has not caught up with a move, so the call is neither served nor denied.
pub(crate) fn pending(tool: &str, workspace_id: &str, pending: PendingWorkspace) -> ToolError {
    ToolError {
        code: AUTHORIZATION_UNAVAILABLE.into(),
        message: format!(
            "that workspace's placement is still being projected; nothing was changed, retry in {} seconds",
            pending.retry_after_seconds
        ),
        details: Some(json!({
            "tool": tool,
            "workspace_id": workspace_id,
            "reason": "placement_pending",
            "retry_after_seconds": pending.retry_after_seconds,
        })),
    }
}

fn unavailable(tool: &str, retry_after_seconds: u64) -> ToolError {
    ToolError {
        code: AUTHORIZATION_UNAVAILABLE.into(),
        message: format!(
            "this connection's authorization could not be read; nothing was changed, retry in {retry_after_seconds} seconds"
        ),
        details: Some(json!({
            "tool": tool,
            "reason": "authority_unavailable",
            "retry_after_seconds": retry_after_seconds,
        })),
    }
}

fn revoked(tool: &str) -> ToolError {
    ToolError {
        code: AUTHORIZATION_REVOKED.into(),
        message: "this connection no longer authorizes this call".into(),
        details: Some(json!({ "tool": tool })),
    }
}

#[cfg(test)]
mod tests;
