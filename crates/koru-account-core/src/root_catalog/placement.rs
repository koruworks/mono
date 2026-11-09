// SPDX-License-Identifier: MPL-2.0

//! Ownership and membership decoupled from placement (TECHNICAL.final §3.2
//! "Workspace UUID is content identity; Profile placement is personal
//! organization"; §7.3 "Content admission verifies workspace UUID, account
//! membership, dataset/incarnation and local policy. The former Profile is
//! not a prerequisite. Current Profile units sync only when independently
//! eligible under 4.4"; "Hosted storage must separate owner
//! account/workspace membership from placement. Legacy Profile owner/member
//! columns are transition projections only; do not mint a new
//! replica/incarnation or expand scopes to fix a mismatch").
//!
//! Three rules, each a pure function:
//!
//! - [`decide_content_admission`] admits a Workspace's content on its id,
//!   the account's live membership, the incarnation the client expects and
//!   the client's own policy. There is no Profile input; the type cannot
//!   express a former-Profile prerequisite.
//! - [`profile_units_eligible`] is §4.4's Identity/Private rule read from
//!   the committed projection: at least one Cloud-enabled Workspace
//!   currently placed under the Profile. Moving the last one out closes it;
//!   the destination opens under its own policy.
//! - [`apply_move`] records a placement change against a Workspace's
//!   content identity, which it takes by shared reference: a move cannot
//!   touch the replica, the incarnation or the member set, and the test
//!   that proves it asserts equality.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::projection::{DiscoveryProjection, SubjectLifecycle};
use crate::workspace_lifecycle::WorkspaceCloudState;
use crate::{KoruIdentityId, ProfileId, WorkspaceId};

/// Where a Workspace is placed, as the hosted side projects it. Carried on
/// the v2 descriptor and the v2 candidate beside — never inside — the
/// membership facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WorkspacePlacement {
    /// `None` is explicitly unassigned.
    pub placement: Option<ProfileId>,
    /// The unit revision at which this placement was committed to the
    /// projection; a Profile-grant cache invalidates on it.
    pub placement_revision: i64,
}

/// The account's live membership of a Workspace, as the hosted tables hold
/// it. A member Profile is not here on purpose: membership is an account
/// fact, and which Profile a Workspace is placed under is the catalog's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipFact {
    pub account_id: KoruIdentityId,
    pub membership_epoch: u64,
    pub active: bool,
}

/// What the hosted side knows about one Workspace's content when a device
/// asks to sync it: identity, lifecycle and the asking account's membership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostedContentFacts {
    pub workspace_id: WorkspaceId,
    pub state: WorkspaceCloudState,
    pub incarnation_id: Uuid,
    pub logical_replica_id: Uuid,
    pub membership: Option<MembershipFact>,
    pub authorization_version: u64,
    pub cloud_access_epoch: u64,
}

/// What the device contributes: the incarnation it holds, if any, and
/// whether its own transport policy for the Workspace admits the Cloud path
/// (§4.4: a Cloud Workspace moved into a LocalOnly or P2P Profile keeps its
/// choice, reports `ceiling_mismatch`, and the ceiling stops this path).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientContentPolicy {
    pub expected_incarnation_id: Option<Uuid>,
    pub allows_cloud: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentRefusal {
    /// No live membership of this account on this Workspace.
    WorkspaceNotAuthorized,
    CloudDisabled,
    CloudDeleting,
    CloudAbsent,
    /// The device holds another incarnation; it must re-bootstrap, never
    /// merge two lineages.
    CloudIncarnationStale,
    /// The device's own policy closes the Cloud path for this Workspace.
    /// Reported, never overridden: `all_data` is authorization, not a
    /// transport-policy override.
    PolicyRefusesCloud,
}

impl ContentRefusal {
    pub const fn code(self) -> &'static str {
        match self {
            Self::WorkspaceNotAuthorized => "workspace_not_authorized",
            Self::CloudDisabled => "cloud_disabled",
            Self::CloudDeleting => "cloud_deleting",
            Self::CloudAbsent => "cloud_absent",
            Self::CloudIncarnationStale => "cloud_incarnation_stale",
            Self::PolicyRefusesCloud => "mode_refuses_transport",
        }
    }
}

/// What an admitted content session is fenced on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentAdmission {
    pub workspace_id: WorkspaceId,
    pub incarnation_id: Uuid,
    pub logical_replica_id: Uuid,
    pub membership_epoch: u64,
    pub authorization_version: u64,
    pub cloud_access_epoch: u64,
}

/// §7.3's content admission. Order: membership, lifecycle, incarnation,
/// policy — so a refusal names the first fact that fails and never leaks a
/// later one about a Workspace the account is not a member of.
pub fn decide_content_admission(
    account: KoruIdentityId,
    hosted: &HostedContentFacts,
    client: ClientContentPolicy,
) -> Result<ContentAdmission, ContentRefusal> {
    let membership = hosted
        .membership
        .filter(|membership| membership.active && membership.account_id == account)
        .ok_or(ContentRefusal::WorkspaceNotAuthorized)?;
    match hosted.state {
        WorkspaceCloudState::Enabled => {}
        WorkspaceCloudState::Disabled => return Err(ContentRefusal::CloudDisabled),
        WorkspaceCloudState::Deleting => return Err(ContentRefusal::CloudDeleting),
        WorkspaceCloudState::Absent => return Err(ContentRefusal::CloudAbsent),
    }
    if let Some(expected) = client.expected_incarnation_id
        && expected != hosted.incarnation_id
    {
        return Err(ContentRefusal::CloudIncarnationStale);
    }
    if !client.allows_cloud {
        return Err(ContentRefusal::PolicyRefusesCloud);
    }
    Ok(ContentAdmission {
        workspace_id: hosted.workspace_id,
        incarnation_id: hosted.incarnation_id,
        logical_replica_id: hosted.logical_replica_id,
        membership_epoch: membership.membership_epoch,
        authorization_version: hosted.authorization_version,
        cloud_access_epoch: hosted.cloud_access_epoch,
    })
}

/// §4.4's Identity/Private eligibility from the committed projection:
/// `profile` is live and at least one of `cloud_enabled` is a live Workspace
/// currently placed under it. Recomputed at the projection's revision; a
/// permit issued under an earlier answer is fenced by its generation.
pub fn profile_units_eligible(
    profile: ProfileId,
    projection: &DiscoveryProjection,
    cloud_enabled: &BTreeSet<WorkspaceId>,
) -> bool {
    projection
        .profile(profile)
        .is_some_and(|p| p.lifecycle == SubjectLifecycle::Live)
        && projection
            .placed_under(Some(profile))
            .any(|workspace| cloud_enabled.contains(&workspace))
}

/// The content identity a move must not touch: the replica, the lineage and
/// the account members. Everything the hosted `cloud_workspaces` /
/// `cloud_replicas` rows own and the catalog does not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceContentIdentity {
    pub workspace_id: WorkspaceId,
    pub incarnation_id: Uuid,
    pub logical_replica_id: Uuid,
    pub membership_epoch: u64,
    pub members: BTreeSet<KoruIdentityId>,
}

/// What a move did to the placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveOutcome {
    Moved {
        from: Option<ProfileId>,
        to: Option<ProfileId>,
    },
    /// Already there; the revision does not move either.
    Unchanged,
}

/// Record a placement change at `revision`. `content` is read, never
/// written: a move preserves replica and incarnation and never expands
/// membership, by construction. The projection's own watermark rule decides
/// whether `revision` is current; this applies what it was told.
pub fn apply_move(
    content: &WorkspaceContentIdentity,
    placement: &mut WorkspacePlacement,
    to: Option<ProfileId>,
    revision: i64,
) -> MoveOutcome {
    debug_assert!(!content.members.is_empty() || content.membership_epoch > 0);
    if placement.placement == to {
        return MoveOutcome::Unchanged;
    }
    let from = placement.placement;
    placement.placement = to;
    placement.placement_revision = revision;
    MoveOutcome::Moved { from, to }
}
