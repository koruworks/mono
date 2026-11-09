// SPDX-License-Identifier: MPL-2.0

//! Grant evaluation against **current placement** (root-catalog
//! `docs/features/root-catalog/TECHNICAL.final.md` §3.9, §7.3 last paragraph,
//! §7.4 and §11.4; the roadmap's Plan 3.3).
//!
//! A connection holds grants of two kinds. A **Profile grant** follows the
//! Profile's current placement: it yields the live, Cloud-enabled Workspaces
//! the committed discovery projection places under that Profile, and loses
//! the ones moved out. A **Workspace grant** is pinned to the Workspace UUID
//! and follows the Workspace wherever it is placed. Neither yields a Profile
//! catalog unit, a personal unit or anything outside the account's
//! membership: the result is a set of Workspace candidates and nothing else,
//! which the types enforce.
//!
//! The inputs are the account's current authorization (the Cloud-enabled
//! Workspaces under live membership, as [`CloudWorkspaceCandidate`]s), the
//! committed [`DiscoveryProjection`] and the admitted unit frontiers the
//! request was made against. The candidate's `profile_id` is **not** read:
//! it is a projection hint for the v1 wire, and the placement here comes
//! from the projection row.
//!
//! **Lag is pending, never a fallback.** A Workspace whose projection row is
//! missing, or whose admitted unit revision is above the projection
//! watermark, is answered as [`GrantPending`] with a retry hint. It is
//! neither granted from the hint nor denied: the host answers the typed
//! retry envelope and asks again.
//!
//! **The cache key** is [`GrantEvaluation`]: every candidate's membership
//! facts and, for a connection with a Profile grant, each Workspace's
//! placement and placement revision. [`GrantEvaluation::is_current`] rebuilds
//! it from the current inputs and compares, so a cached session's set is
//! used only while nothing it was computed from has moved — a move changes
//! `placement_revision` and nothing else (`projection`), a membership change
//! moves its epoch. Per-unit revisions do not add up to one scalar, which is
//! why the key is a map and not a number.

use std::collections::{BTreeMap, BTreeSet};

use crate::root_catalog::{CloudUnitRef, DiscoveryProjection, SubjectLifecycle, UnitFrontier};
use crate::{ProfileId, WorkspaceId};

use super::{CloudWorkspaceCandidate, ProfileAccess, ProfileDecision};

/// How long a caller waits before asking again after a [`GrantPending`]
/// answer. The projection job runs right after a unit is stored, so lag is
/// ordinarily under a second; five seconds keeps a retry from racing it.
pub const GRANT_PENDING_RETRY_SECONDS: i64 = 5;

/// What one grant is about (§3.9): a Profile, whose set follows placement,
/// or one Workspace, pinned to its UUID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GrantScope {
    Profile(ProfileId),
    Workspace(WorkspaceId),
}

/// What the person decided for one Workspace, pinned to its UUID. The
/// Workspace counterpart of [`ProfileDecision`]; `access` is the same
/// allowed / not-allowed answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceDecision {
    pub workspace_id: WorkspaceId,
    pub access: ProfileAccess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceDecisionRecord {
    pub workspace_id: WorkspaceId,
    pub access: ProfileAccess,
    pub decided_at_millis: i64,
}

/// One decision of either kind, as the evaluation reads them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrantDecision {
    pub scope: GrantScope,
    pub access: ProfileAccess,
}

/// The connection's decisions as one list: Profile decisions first, then
/// Workspace decisions. Order does not affect the evaluation.
pub fn grant_decisions(
    profiles: &[ProfileDecision],
    workspaces: &[WorkspaceDecision],
) -> Vec<GrantDecision> {
    profiles
        .iter()
        .map(|decision| GrantDecision {
            scope: GrantScope::Profile(decision.profile_id),
            access: decision.access,
        })
        .chain(workspaces.iter().map(|decision| GrantDecision {
            scope: GrantScope::Workspace(decision.workspace_id),
            access: decision.access,
        }))
        .collect()
}

/// What a connection brings to an evaluation. [`super::AppConnectionRecord::grant_request`]
/// builds it from a stored connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantRequest {
    pub grants: Vec<GrantDecision>,
    /// A Cloud Profile with no Profile decision is included when set
    /// (TECHNICAL.final §6.1's later flag).
    pub allow_profiles_enabled_later: bool,
    /// A revoked connection yields nothing.
    pub live: bool,
}

impl GrantRequest {
    fn profile_access(&self, profile: ProfileId) -> Option<ProfileAccess> {
        self.grants.iter().find_map(|grant| match grant.scope {
            GrantScope::Profile(granted) if granted == profile => Some(grant.access),
            _ => None,
        })
    }

    fn workspace_access(&self, workspace: WorkspaceId) -> Option<ProfileAccess> {
        self.grants.iter().find_map(|grant| match grant.scope {
            GrantScope::Workspace(granted) if granted == workspace => Some(grant.access),
            _ => None,
        })
    }

    /// Whether any grant follows placement. A connection with only
    /// Workspace grants is indifferent to moves, and its cache key says so.
    pub fn follows_placement(&self) -> bool {
        self.grants
            .iter()
            .any(|grant| matches!(grant.scope, GrantScope::Profile(_)))
    }
}

/// Why a Workspace's grant cannot be decided yet (§7.4: "Cloud projection
/// lag can give a typed pending/retry result, not fallback").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PendingReason {
    /// The projection has never read this Workspace's unit.
    WorkspaceNotProjected,
    /// A newer revision of the unit is admitted than the projection has
    /// committed: a move the projection has not caught up with.
    ProjectionLag { watermark: i64, admitted: i64 },
    /// The Workspace is placed under a Profile whose unit the projection
    /// has not read. A Workspace under a Profile Cloud never admitted is
    /// held at admission, so this is lag, not a hidden Profile.
    ProfileNotProjected { profile_id: ProfileId },
}

/// A Workspace neither granted nor refused: ask again after
/// `retry_after_seconds`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GrantPending {
    pub workspace_id: WorkspaceId,
    pub retry_after_seconds: i64,
    pub reason: PendingReason,
}

/// The projection's facts about one Workspace as the key records them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProjectedPlacement {
    /// No decision could be made for this Workspace at evaluation.
    Pending,
    Projected {
        /// `None` is explicitly unassigned. Zeroed with `placement_revision`
        /// and `profile_lifecycle` when the connection has no Profile grant:
        /// a move then changes nothing about its access.
        profile_id: Option<ProfileId>,
        placement_revision: i64,
        lifecycle: SubjectLifecycle,
        /// The placing Profile's projected lifecycle; `None` while the
        /// projection has no row for it (or the Workspace is unassigned).
        profile_lifecycle: Option<SubjectLifecycle>,
    },
}

/// Everything about one candidate Workspace the set depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorkspaceRevision {
    pub membership_epoch: i64,
    pub authorization_version: i64,
    pub cloud_access_epoch: i64,
    pub documents_write: bool,
    pub placement: ProjectedPlacement,
}

/// The cache key of one evaluation: what [`resolve_workspaces`] read, for
/// every candidate Workspace, granted or not — a Workspace moved *into* a
/// granted Profile changes the set as much as one moved out.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct GrantEvaluation {
    pub revisions: BTreeMap<WorkspaceId, WorkspaceRevision>,
    /// Whether placement is part of this key (the connection has a Profile
    /// grant). When false, only membership facts and lifecycle are compared.
    pub follows_placement: bool,
}

impl GrantEvaluation {
    /// Whether a set computed under this key may still be used: nothing
    /// relevant has moved in the projection or the membership facts since.
    /// A `false` answer means the host re-resolves before serving; it never
    /// serves the cached set to find out.
    pub fn is_current(
        &self,
        projection: &DiscoveryProjection,
        candidates: &[CloudWorkspaceCandidate],
        admitted: &[UnitFrontier],
    ) -> bool {
        *self == key(projection, candidates, admitted, self.follows_placement)
    }
}

/// What one evaluation answers: the granted Workspaces, the ones that
/// cannot be decided yet, and the key the host caches the answer under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedGrants {
    /// The granted Workspaces, in the candidates' order. Each candidate's
    /// `profile_id` hint is rewritten to its projected placement when the
    /// projection places it under a Profile, so a v1 reader sees current
    /// placement; an unassigned Workspace keeps the hint it came with,
    /// because the v1 shape has no way to say "none".
    pub workspaces: Vec<CloudWorkspaceCandidate>,
    pub pending: Vec<GrantPending>,
    pub evaluation: GrantEvaluation,
}

/// **The resolved Workspace set, from current placement** (§3.9, §7.4).
/// For each candidate — already Cloud-enabled under a live membership —
/// in order:
///
/// 1. no projection row, or the admitted revision is above the watermark:
///    [`GrantPending`];
/// 2. a Workspace decision for its UUID: `allowed` includes it wherever it
///    is placed, `not_allowed` excludes it, and both override a Profile
///    grant, because an explicit answer about this Workspace beats an
///    implicit one about its Profile;
/// 3. otherwise, its projected placement decides: a Profile decided
///    `allowed` includes it, `not_allowed` excludes it, no decision includes
///    it only under `allow_profiles_enabled_later`; an explicitly unassigned
///    Workspace is under no Profile and is not included by any Profile
///    grant. A Profile grant yields only Workspaces whose root lifecycle is
///    live and whose Profile is projected live; a deleted Profile grants
///    nothing.
///
/// A revoked connection yields nothing. The order of `candidates` is kept.
pub fn resolve_workspaces(
    candidates: Vec<CloudWorkspaceCandidate>,
    request: &GrantRequest,
    projection: &DiscoveryProjection,
    admitted: &[UnitFrontier],
) -> ResolvedGrants {
    let evaluation = key(
        projection,
        &candidates,
        admitted,
        request.follows_placement(),
    );
    if !request.live {
        return ResolvedGrants {
            workspaces: Vec::new(),
            pending: Vec::new(),
            evaluation,
        };
    }
    let mut workspaces = Vec::new();
    let mut pending = Vec::new();
    for mut candidate in candidates {
        let row = match projected(projection, admitted, candidate.workspace_id) {
            Ok(row) => row,
            Err(reason) => {
                pending.push(GrantPending {
                    workspace_id: candidate.workspace_id,
                    retry_after_seconds: GRANT_PENDING_RETRY_SECONDS,
                    reason,
                });
                continue;
            }
        };
        if row.lifecycle != SubjectLifecycle::Live {
            continue;
        }
        if let Some(profile) = row.placement {
            candidate.profile_id = profile;
        }
        let granted = match request.workspace_access(candidate.workspace_id) {
            Some(access) => access == ProfileAccess::Allowed,
            None => match row.placement {
                None => false,
                Some(profile) => {
                    let Some(profile_lifecycle) = row.profile_lifecycle else {
                        pending.push(GrantPending {
                            workspace_id: candidate.workspace_id,
                            retry_after_seconds: GRANT_PENDING_RETRY_SECONDS,
                            reason: PendingReason::ProfileNotProjected {
                                profile_id: profile,
                            },
                        });
                        continue;
                    };
                    profile_lifecycle == SubjectLifecycle::Live
                        && match request.profile_access(profile) {
                            Some(access) => access == ProfileAccess::Allowed,
                            None => request.allow_profiles_enabled_later,
                        }
                }
            },
        };
        if granted {
            workspaces.push(candidate);
        }
    }
    ResolvedGrants {
        workspaces,
        pending,
        evaluation,
    }
}

/// The Profiles the consent page and the connections list show as
/// **Cloud-syncing**: a Profile is a Cloud Profile exactly when the committed
/// projection places at least one Cloud-enabled Workspace under it and the
/// Profile itself is projected live. An empty Cloud-mode Profile is still an
/// active Profile the account holds — listed, not syncing — rather than
/// absent.
pub fn cloud_syncing_profiles(
    projection: &DiscoveryProjection,
    candidates: &[CloudWorkspaceCandidate],
) -> BTreeSet<ProfileId> {
    let cloud_enabled: BTreeSet<WorkspaceId> = candidates
        .iter()
        .map(|candidate| candidate.workspace_id)
        .collect();
    projection
        .profiles()
        .filter(|(_, profile)| profile.lifecycle == SubjectLifecycle::Live)
        .map(|(profile, _)| profile)
        .filter(|profile| {
            projection
                .placed_under(Some(*profile))
                .any(|workspace| cloud_enabled.contains(&workspace))
        })
        .collect()
}

struct Row {
    placement: Option<ProfileId>,
    placement_revision: i64,
    lifecycle: SubjectLifecycle,
    /// The placing Profile's projected lifecycle, `None` while it has no
    /// row or the Workspace is unassigned.
    profile_lifecycle: Option<SubjectLifecycle>,
}

/// The projection's row for `workspace`, or why it cannot be read yet.
fn projected(
    projection: &DiscoveryProjection,
    admitted: &[UnitFrontier],
    workspace: WorkspaceId,
) -> Result<Row, PendingReason> {
    let unit = CloudUnitRef::WorkspaceCatalog {
        subject_id: workspace,
    };
    let row = projection
        .workspace(workspace)
        .ok_or(PendingReason::WorkspaceNotProjected)?;
    if let Some(frontier) = admitted.iter().find(|frontier| frontier.unit == unit)
        && frontier.revision > row.watermark
    {
        return Err(PendingReason::ProjectionLag {
            watermark: row.watermark,
            admitted: frontier.revision,
        });
    }
    Ok(Row {
        placement: row.placement,
        placement_revision: row.placement_revision,
        lifecycle: row.lifecycle,
        profile_lifecycle: row
            .placement
            .and_then(|profile| projection.profile(profile))
            .map(|profile| profile.lifecycle),
    })
}

fn key(
    projection: &DiscoveryProjection,
    candidates: &[CloudWorkspaceCandidate],
    admitted: &[UnitFrontier],
    follows_placement: bool,
) -> GrantEvaluation {
    let revisions = candidates
        .iter()
        .map(|candidate| {
            let placement = match projected(projection, admitted, candidate.workspace_id) {
                Err(_) => ProjectedPlacement::Pending,
                Ok(row) if follows_placement => ProjectedPlacement::Projected {
                    profile_id: row.placement,
                    placement_revision: row.placement_revision,
                    lifecycle: row.lifecycle,
                    profile_lifecycle: row.profile_lifecycle,
                },
                Ok(row) => ProjectedPlacement::Projected {
                    profile_id: None,
                    placement_revision: 0,
                    lifecycle: row.lifecycle,
                    profile_lifecycle: None,
                },
            };
            (
                candidate.workspace_id,
                WorkspaceRevision {
                    membership_epoch: candidate.membership_epoch,
                    authorization_version: candidate.authorization_version,
                    cloud_access_epoch: candidate.cloud_access_epoch,
                    documents_write: candidate.documents_write,
                    placement,
                },
            )
        })
        .collect();
    GrantEvaluation {
        revisions,
        follows_placement,
    }
}

#[cfg(test)]
mod tests;
