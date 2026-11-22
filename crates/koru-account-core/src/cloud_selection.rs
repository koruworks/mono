// SPDX-License-Identifier: MPL-2.0

//! Bounded Cloud discovery and Profile selection ports.
//!
//! A native device enumerates the Workspaces its current session may sync (a
//! page of eligibility pairs, never names or routes) and selects one Profile
//! this device holds. Selection *is* the admission: the owner session writes
//! the existing `provisioned` device/Profile binding and invalidates the
//! Profile's authority in the same transaction. No provisioning token, receipt
//! or worker availability check exists at this boundary. Every identifier a
//! repository trusts arrives from the verified native session, never from the
//! request body.

use uuid::Uuid;

use crate::{
    AccountSessionId, AuthIdentityId, DeviceId, KoruIdentityId, PortFuture, ProfileId, WorkspaceId,
};

/// Fixed §6 page size. A repository reads one more row to learn whether a
/// following page exists; it never returns that row.
pub const CLOUD_WORKSPACE_PAGE_SIZE: usize = 64;

/// A discovery position. It is an untrusted cursor, never authority: every
/// page rechecks current access from the live session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudWorkspaceCursor {
    pub profile_id: ProfileId,
    pub workspace_id: WorkspaceId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudWorkspaceDiscovery {
    pub koru_identity_id: KoruIdentityId,
    pub authenticated_by: AuthIdentityId,
    pub session_id: AccountSessionId,
    pub device_id: DeviceId,
    pub now_millis: i64,
    pub after: Option<CloudWorkspaceCursor>,
}

/// One currently authorized membership pair. The Profile is the membership
/// Profile, not necessarily the Workspace owner; the same Workspace under a
/// different authorized Profile is a distinct eligibility context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudWorkspaceCandidate {
    pub profile_id: ProfileId,
    pub workspace_id: WorkspaceId,
    pub workspace_incarnation_id: Uuid,
    pub authorization_version: i64,
    pub membership_epoch: i64,
    pub cloud_access_epoch: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudWorkspacePage {
    pub workspaces: Vec<CloudWorkspaceCandidate>,
    /// The last returned key iff a further qualifying row exists.
    pub next: Option<CloudWorkspaceCursor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectProfileCloud {
    pub koru_identity_id: KoruIdentityId,
    pub authenticated_by: AuthIdentityId,
    pub session_id: AccountSessionId,
    pub device_id: DeviceId,
    pub profile_id: ProfileId,
    /// Carried for older clients. Selection does not read it: the live owner
    /// session is the grant, whether or not this Workspace is already a member.
    pub workspace_id: WorkspaceId,
    /// Sampled before any lock. The repository re-samples through its clock
    /// after the locks are held and validates expiry against that later time.
    pub now_millis: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudSelectionOutcome {
    /// The binding was inserted or moved to `provisioned`; Profile authority
    /// was invalidated in the same transaction.
    Selected,
    /// The durable row was already `provisioned`. Nothing changed: no epoch,
    /// timestamp or proof tuple moved.
    AlreadySelected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudSelectionError {
    /// Missing, expired or revoked principal, Profile edge, membership or
    /// scope; a revoked or foreign binding. Never an empty success.
    PermissionDenied,
    Unavailable,
    Corrupt,
}

/// What a fresh device asks before it can build a
/// Workspace permit. Every identity here comes from the verified native
/// session except the requested Profile and Workspace pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudWorkspaceDescriptorQuery {
    pub koru_identity_id: KoruIdentityId,
    pub authenticated_by: AuthIdentityId,
    pub session_id: AccountSessionId,
    pub device_id: DeviceId,
    pub now_millis: i64,
    pub profile_id: ProfileId,
    pub workspace_id: WorkspaceId,
}

/// Identity-only bootstrap facts for one authorized Workspace: the replica,
/// its lineage and the manifest's original Store document id retained by
/// the Cloud catalog (ADR-358). No names, appearance, content or credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudWorkspaceDescriptor {
    pub profile_id: ProfileId,
    pub workspace_id: WorkspaceId,
    pub workspace_incarnation_id: Uuid,
    pub logical_replica_id: Uuid,
    pub lineage_id: Uuid,
    pub manifest_document_id: Uuid,
    pub authorization_version: i64,
    pub membership_epoch: i64,
    pub cloud_access_epoch: i64,
}

/// Millisecond clock the selection transaction re-samples after its locks.
pub trait SelectionClock: Send + Sync {
    fn now_millis(&self) -> i64;
}

impl<T: crate::Clock> SelectionClock for T {
    fn now_millis(&self) -> i64 {
        crate::Clock::now_millis(self)
    }
}

pub trait CloudSelectionRepository: Send + Sync {
    fn discover_cloud_workspaces<'a>(
        &'a self,
        query: CloudWorkspaceDiscovery,
    ) -> PortFuture<'a, Result<CloudWorkspacePage, CloudSelectionError>>;

    fn select_profile_cloud<'a>(
        &'a self,
        request: SelectProfileCloud,
        clock: &'a dyn SelectionClock,
    ) -> PortFuture<'a, Result<CloudSelectionOutcome, CloudSelectionError>>;

    /// `PermissionDenied` for a principal, Profile edge, binding, membership
    /// or scope that does not currently authorize the pair; `Unavailable`
    /// for an authorized pair whose replica or active manifest mapping does
    /// not exist yet. Never a fabricated id.
    fn describe_cloud_workspace<'a>(
        &'a self,
        query: CloudWorkspaceDescriptorQuery,
    ) -> PortFuture<'a, Result<CloudWorkspaceDescriptor, CloudSelectionError>>;
}
