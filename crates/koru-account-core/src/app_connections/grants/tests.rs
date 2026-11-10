// SPDX-License-Identifier: MPL-2.0

//! Memory only: projections reduced from in-memory catalog units built by
//! koru-store-core's own constructors, no disk, no Store, no clock.

use koru_store_core::DeletionId;
use koru_store_core::root_catalog::{DeletionRecord, ProfileCatalogUnit, WorkspaceCatalogUnit};
use uuid::Uuid;

use super::*;
use crate::root_catalog::ProjectionJob;

const WRITER: u64 = 7;

fn candidate(hint: ProfileId, workspace: WorkspaceId) -> CloudWorkspaceCandidate {
    CloudWorkspaceCandidate {
        profile_id: hint,
        workspace_id: workspace,
        documents_write: true,
        workspace_incarnation_id: Uuid::new_v4(),
        authorization_version: 1,
        membership_epoch: 1,
        cloud_access_epoch: 1,
        manifest_document_id: None,
    }
}

fn profile_grant(profile: ProfileId, access: ProfileAccess) -> GrantDecision {
    GrantDecision {
        scope: GrantScope::Profile(profile),
        access,
    }
}

fn workspace_grant(workspace: WorkspaceId, access: ProfileAccess) -> GrantDecision {
    GrantDecision {
        scope: GrantScope::Workspace(workspace),
        access,
    }
}

fn request(grants: Vec<GrantDecision>, allow_later: bool) -> GrantRequest {
    GrantRequest {
        grants,
        allow_profiles_enabled_later: allow_later,
        live: true,
    }
}

/// A catalog as the service holds it: the committed projection and the
/// admitted frontier of every unit, advanced together or apart.
#[derive(Default)]
struct Catalog {
    projection: DiscoveryProjection,
    admitted: Vec<UnitFrontier>,
    profiles: Vec<(ProfileId, ProfileCatalogUnit)>,
    workspaces: Vec<(WorkspaceId, WorkspaceCatalogUnit)>,
}

impl Catalog {
    fn unit_ref(&self, workspace: WorkspaceId) -> CloudUnitRef {
        CloudUnitRef::WorkspaceCatalog {
            subject_id: workspace,
        }
    }

    fn frontier(&mut self, unit: CloudUnitRef, revision: i64) {
        match self.admitted.iter_mut().find(|f| f.unit == unit) {
            Some(frontier) => frontier.revision = revision,
            None => self.admitted.push(UnitFrontier { unit, revision }),
        }
    }

    fn apply(&mut self, unit: CloudUnitRef, snapshot: &[u8], revision: i64) {
        self.projection
            .apply(&ProjectionJob {
                unit,
                snapshot,
                revision,
            })
            .unwrap();
    }

    /// A live Profile, admitted and projected at `revision`.
    fn profile(&mut self, profile: ProfileId, revision: i64) {
        let unit = ProfileCatalogUnit::create(profile, WRITER).unwrap();
        let snapshot = unit.snapshot().unwrap();
        let address = CloudUnitRef::ProfileCatalog {
            subject_id: profile,
        };
        self.frontier(address, revision);
        self.apply(address, &snapshot, revision);
        self.profiles.push((profile, unit));
    }

    /// Delete a Profile: admitted and projected at `revision`.
    fn delete_profile(&mut self, profile: ProfileId, revision: i64) {
        let unit = &self
            .profiles
            .iter()
            .find(|(id, _)| *id == profile)
            .unwrap()
            .1;
        unit.record_deletion(DeletionId::new(), &DeletionRecord::new(1_000, None))
            .unwrap();
        let snapshot = unit.snapshot().unwrap();
        let address = CloudUnitRef::ProfileCatalog {
            subject_id: profile,
        };
        self.frontier(address, revision);
        self.apply(address, &snapshot, revision);
    }

    /// A live Workspace placed under `placement`, admitted and projected at
    /// `revision`.
    fn workspace(&mut self, workspace: WorkspaceId, placement: Option<ProfileId>, revision: i64) {
        let unit = WorkspaceCatalogUnit::create(workspace, WRITER).unwrap();
        unit.set_placement(placement, Uuid::new_v4()).unwrap();
        let snapshot = unit.snapshot().unwrap();
        let address = self.unit_ref(workspace);
        self.frontier(address, revision);
        self.apply(address, &snapshot, revision);
        self.workspaces.push((workspace, unit));
    }

    /// Move a Workspace: the unit is **admitted** at `revision` (the device
    /// published it) but the projection has not read it yet. Returns the
    /// snapshot for [`Self::project`].
    fn admit_move(
        &mut self,
        workspace: WorkspaceId,
        to: Option<ProfileId>,
        revision: i64,
    ) -> Vec<u8> {
        let unit = &self
            .workspaces
            .iter()
            .find(|(id, _)| *id == workspace)
            .unwrap()
            .1;
        unit.set_placement(to, Uuid::new_v4()).unwrap();
        let snapshot = unit.snapshot().unwrap();
        let address = self.unit_ref(workspace);
        self.frontier(address, revision);
        snapshot
    }

    /// Commit the projection job for a unit the device already published.
    fn project(&mut self, workspace: WorkspaceId, snapshot: &[u8], revision: i64) {
        let address = self.unit_ref(workspace);
        self.apply(address, snapshot, revision);
    }

    fn trash_workspace(&mut self, workspace: WorkspaceId, revision: i64) {
        let unit = &self
            .workspaces
            .iter()
            .find(|(id, _)| *id == workspace)
            .unwrap()
            .1;
        unit.record_deletion(DeletionId::new(), &DeletionRecord::new(2_000, None))
            .unwrap();
        let snapshot = unit.snapshot().unwrap();
        let address = self.unit_ref(workspace);
        self.frontier(address, revision);
        self.apply(address, &snapshot, revision);
    }

    fn resolve(
        &self,
        candidates: &[CloudWorkspaceCandidate],
        request: &GrantRequest,
    ) -> ResolvedGrants {
        resolve_workspaces(
            candidates.to_vec(),
            request,
            &self.projection,
            &self.admitted,
        )
    }
}

fn ids(resolved: &ResolvedGrants) -> Vec<WorkspaceId> {
    resolved
        .workspaces
        .iter()
        .map(|candidate| candidate.workspace_id)
        .collect()
}

/// §3.9: a Profile grant follows placement. Move into the granted Profile
/// and the Workspace appears — **after the projection commit, not before**
/// (pending in between); move out and it disappears. The hint on the
/// candidate never decides.
#[test]
fn a_profile_grant_follows_current_placement_after_the_projection_commit() {
    let (work, personal) = (ProfileId::new(), ProfileId::new());
    let (w_work, w_personal) = (WorkspaceId::new(), WorkspaceId::new());
    let mut catalog = Catalog::default();
    catalog.profile(work, 1);
    catalog.profile(personal, 1);
    catalog.workspace(w_work, Some(work), 1);
    catalog.workspace(w_personal, Some(personal), 1);
    // The hints say the opposite of the projection, on purpose.
    let candidates = [candidate(personal, w_work), candidate(work, w_personal)];
    let grant = request(vec![profile_grant(work, ProfileAccess::Allowed)], false);

    let before = catalog.resolve(&candidates, &grant);
    assert_eq!(
        ids(&before),
        vec![w_work],
        "the projection decides, never the hint"
    );
    assert!(before.pending.is_empty());
    assert_eq!(
        before.workspaces[0].profile_id, work,
        "the answered hint is the projected placement"
    );

    // The person moves the personal Workspace into Work on a device. The
    // unit is admitted (revision 2) before the projection reads it.
    let moved = catalog.admit_move(w_personal, Some(work), 2);
    let lagging = catalog.resolve(&candidates, &grant);
    assert_eq!(ids(&lagging), vec![w_work], "not yet granted from the hint");
    assert_eq!(
        lagging.pending,
        vec![GrantPending {
            workspace_id: w_personal,
            retry_after_seconds: GRANT_PENDING_RETRY_SECONDS,
            reason: PendingReason::ProjectionLag {
                watermark: 1,
                admitted: 2
            },
        }]
    );
    assert!(
        !before
            .evaluation
            .is_current(&catalog.projection, &candidates, &catalog.admitted),
        "a cached set must re-resolve once a newer unit is admitted"
    );

    catalog.project(w_personal, &moved, 2);
    let after = catalog.resolve(&candidates, &grant);
    assert_eq!(ids(&after), vec![w_work, w_personal]);
    assert!(after.pending.is_empty());

    // And out again: the Workspace leaves the grant with the commit.
    let out = catalog.admit_move(w_work, Some(personal), 3);
    catalog.project(w_work, &out, 3);
    let gone = catalog.resolve(&candidates, &grant);
    assert_eq!(ids(&gone), vec![w_personal]);
    assert!(
        !after
            .evaluation
            .is_current(&catalog.projection, &candidates, &catalog.admitted)
    );
    assert!(
        gone.evaluation
            .is_current(&catalog.projection, &candidates, &catalog.admitted)
    );
}

/// §3.9: a Workspace grant is pinned to the UUID. The Workspace stays in
/// the set through a move into a Profile the connection was never granted,
/// through a move to explicitly unassigned, and its cache key ignores
/// placement — a move invalidates nothing for a UUID-only connection.
#[test]
fn a_workspace_grant_stays_attached_across_moves_and_ignores_placement_in_its_key() {
    let (a, b) = (ProfileId::new(), ProfileId::new());
    let (w, other) = (WorkspaceId::new(), WorkspaceId::new());
    let mut catalog = Catalog::default();
    catalog.profile(a, 1);
    catalog.profile(b, 1);
    catalog.workspace(w, Some(a), 1);
    catalog.workspace(other, Some(a), 1);
    let candidates = [candidate(a, w), candidate(a, other)];
    let grant = request(vec![workspace_grant(w, ProfileAccess::Allowed)], false);

    let first = catalog.resolve(&candidates, &grant);
    assert_eq!(
        ids(&first),
        vec![w],
        "the other Workspace in A is not granted"
    );
    assert!(!first.evaluation.follows_placement);

    let moved = catalog.admit_move(w, Some(b), 2);
    catalog.project(w, &moved, 2);
    let in_b = catalog.resolve(&candidates, &grant);
    assert_eq!(ids(&in_b), vec![w]);
    assert_eq!(
        in_b.workspaces[0].profile_id, b,
        "the hint follows the projection"
    );
    assert!(
        first
            .evaluation
            .is_current(&catalog.projection, &candidates, &catalog.admitted),
        "a UUID grant's key does not move with placement"
    );

    let unassigned = catalog.admit_move(w, None, 3);
    catalog.project(w, &unassigned, 3);
    let loose = catalog.resolve(&candidates, &grant);
    assert_eq!(
        ids(&loose),
        vec![w],
        "unassigned is still the same Workspace"
    );
    assert_eq!(
        loose.workspaces[0].profile_id, a,
        "no Profile to project: the hint it came with is kept"
    );

    // No Profile grant reaches the unassigned Workspace, not even the later
    // flag: that flag includes `other` under the undecided A, and `w` is
    // under no Profile at all.
    let profile_only = request(vec![profile_grant(b, ProfileAccess::Allowed)], true);
    assert_eq!(
        ids(&catalog.resolve(&candidates, &profile_only)),
        vec![other],
        "unassigned is under no Profile; the later flag reaches only `other`"
    );
}

/// A stale evaluation — older placement revision or membership epoch — is
/// `!is_current`, so a cached session re-resolves; an unchanged catalog
/// keeps it current. A membership change invalidates every grant kind.
#[test]
fn a_stale_evaluation_is_not_current_and_an_unchanged_one_is() {
    let a = ProfileId::new();
    let w = WorkspaceId::new();
    let mut catalog = Catalog::default();
    catalog.profile(a, 1);
    catalog.workspace(w, Some(a), 1);
    let candidates = [candidate(a, w)];
    let profile = request(vec![profile_grant(a, ProfileAccess::Allowed)], false);
    let pinned = request(vec![workspace_grant(w, ProfileAccess::Allowed)], false);

    let by_profile = catalog.resolve(&candidates, &profile);
    let by_uuid = catalog.resolve(&candidates, &pinned);
    assert!(
        by_profile
            .evaluation
            .is_current(&catalog.projection, &candidates, &catalog.admitted)
    );
    assert!(
        by_uuid
            .evaluation
            .is_current(&catalog.projection, &candidates, &catalog.admitted)
    );
    assert_ne!(by_profile.evaluation, by_uuid.evaluation);

    // The membership epoch moves: both keys are stale.
    let mut rejoined = candidates;
    rejoined[0].membership_epoch = 2;
    assert!(
        !by_profile
            .evaluation
            .is_current(&catalog.projection, &rejoined, &catalog.admitted)
    );
    assert!(
        !by_uuid
            .evaluation
            .is_current(&catalog.projection, &rejoined, &catalog.admitted)
    );

    // The authorization version moves, or writes are withdrawn: stale too.
    let mut downgraded = candidates;
    downgraded[0].documents_write = false;
    assert!(
        !by_uuid
            .evaluation
            .is_current(&catalog.projection, &downgraded, &catalog.admitted)
    );

    // A move (placement revision) is stale only for the Profile grant.
    let moved = catalog.admit_move(w, None, 2);
    catalog.project(w, &moved, 2);
    assert!(
        !by_profile
            .evaluation
            .is_current(&catalog.projection, &candidates, &catalog.admitted)
    );
    assert!(
        by_uuid
            .evaluation
            .is_current(&catalog.projection, &candidates, &catalog.admitted)
    );

    // A Workspace trashed in the root leaves every set, and both keys.
    catalog.trash_workspace(w, 3);
    assert!(ids(&catalog.resolve(&candidates, &pinned)).is_empty());
    assert!(
        !by_uuid
            .evaluation
            .is_current(&catalog.projection, &candidates, &catalog.admitted)
    );
}

/// A revoked connection yields nothing, whatever it was granted.
#[test]
fn a_revoked_connection_yields_nothing() {
    let a = ProfileId::new();
    let w = WorkspaceId::new();
    let mut catalog = Catalog::default();
    catalog.profile(a, 1);
    catalog.workspace(w, Some(a), 1);
    let candidates = [candidate(a, w)];
    let revoked = GrantRequest {
        grants: vec![
            profile_grant(a, ProfileAccess::Allowed),
            workspace_grant(w, ProfileAccess::Allowed),
        ],
        allow_profiles_enabled_later: true,
        live: false,
    };
    let resolved = catalog.resolve(&candidates, &revoked);
    assert!(resolved.workspaces.is_empty());
    assert!(resolved.pending.is_empty());
}

/// The later flag, read from placement: an undecided Profile that holds a
/// Cloud-enabled Workspace is included only under the flag; `not_allowed`
/// is never reopened by it; and an explicit Workspace `not_allowed`
/// overrides the Profile it sits in.
#[test]
fn the_later_flag_and_explicit_refusals_resolve_exactly() {
    let (work, personal, third) = (ProfileId::new(), ProfileId::new(), ProfileId::new());
    let (w10, w11, w20, w30) = (
        WorkspaceId::new(),
        WorkspaceId::new(),
        WorkspaceId::new(),
        WorkspaceId::new(),
    );
    let mut catalog = Catalog::default();
    for profile in [work, personal, third] {
        catalog.profile(profile, 1);
    }
    catalog.workspace(w10, Some(work), 1);
    catalog.workspace(w11, Some(work), 1);
    catalog.workspace(w20, Some(personal), 1);
    catalog.workspace(w30, Some(third), 1);
    let candidates = [
        candidate(work, w10),
        candidate(work, w11),
        candidate(personal, w20),
        candidate(third, w30),
    ];
    let grants = vec![
        profile_grant(work, ProfileAccess::Allowed),
        profile_grant(personal, ProfileAccess::NotAllowed),
    ];
    assert_eq!(
        ids(&catalog.resolve(&candidates, &request(grants.clone(), true))),
        vec![w10, w11, w30]
    );
    assert_eq!(
        ids(&catalog.resolve(&candidates, &request(grants.clone(), false))),
        vec![w10, w11]
    );
    assert!(ids(&catalog.resolve(&candidates, &request(Vec::new(), false))).is_empty());

    let mut with_refusal = grants;
    with_refusal.push(workspace_grant(w11, ProfileAccess::NotAllowed));
    assert_eq!(
        ids(&catalog.resolve(&candidates, &request(with_refusal, true))),
        vec![w10, w30],
        "an explicit answer about the Workspace beats the Profile's"
    );
}

/// Type-level: whatever is granted, the answer is Workspace candidates and
/// nothing else — no Profile catalog unit, no personal unit, no scope. A
/// Workspace-only grant does not even learn which Profiles exist: its key
/// carries no Profile id.
#[test]
fn a_workspace_only_grant_never_yields_a_profile_or_personal_unit() {
    let a = ProfileId::new();
    let w = WorkspaceId::new();
    let mut catalog = Catalog::default();
    catalog.profile(a, 1);
    catalog.workspace(w, Some(a), 1);
    // A personal unit is admitted for the Workspace too; it is not a
    // candidate and cannot become one.
    catalog.frontier(CloudUnitRef::PersonalV2 { subject_id: w }, 1);
    let candidates = [candidate(a, w)];
    let resolved = catalog.resolve(
        &candidates,
        &request(vec![workspace_grant(w, ProfileAccess::Allowed)], false),
    );
    let _: &Vec<CloudWorkspaceCandidate> = &resolved.workspaces;
    assert_eq!(ids(&resolved), vec![w]);
    for revision in resolved.evaluation.revisions.values() {
        assert_eq!(
            revision.placement,
            ProjectedPlacement::Projected {
                profile_id: None,
                placement_revision: 0,
                lifecycle: SubjectLifecycle::Live,
                profile_lifecycle: None,
            }
        );
    }
}

/// §7.4: a Workspace the projection has never read is pending, not granted
/// from its hint and not denied; so is one whose Profile row is missing.
#[test]
fn projection_lag_is_a_typed_pending_result_not_a_fallback() {
    let a = ProfileId::new();
    let (projected, fresh) = (WorkspaceId::new(), WorkspaceId::new());
    let mut catalog = Catalog::default();
    catalog.profile(a, 1);
    catalog.workspace(projected, Some(a), 1);
    catalog.frontier(CloudUnitRef::WorkspaceCatalog { subject_id: fresh }, 1);
    let candidates = [candidate(a, projected), candidate(a, fresh)];
    let grant = request(vec![profile_grant(a, ProfileAccess::Allowed)], false);
    let resolved = catalog.resolve(&candidates, &grant);
    assert_eq!(ids(&resolved), vec![projected]);
    assert_eq!(
        resolved.pending,
        vec![GrantPending {
            workspace_id: fresh,
            retry_after_seconds: GRANT_PENDING_RETRY_SECONDS,
            reason: PendingReason::WorkspaceNotProjected,
        }]
    );

    // A Workspace placed under a Profile the projection has no row for.
    let b = ProfileId::new();
    let under_b = WorkspaceId::new();
    catalog.workspace(under_b, Some(b), 1);
    let with_b = [candidate(a, under_b)];
    let resolved = catalog.resolve(
        &with_b,
        &request(vec![profile_grant(b, ProfileAccess::Allowed)], false),
    );
    assert!(resolved.workspaces.is_empty());
    assert_eq!(
        resolved.pending[0].reason,
        PendingReason::ProfileNotProjected { profile_id: b }
    );
    // The same Workspace under a UUID grant needs no Profile row.
    let pinned = catalog.resolve(
        &with_b,
        &request(
            vec![workspace_grant(under_b, ProfileAccess::Allowed)],
            false,
        ),
    );
    assert_eq!(ids(&pinned), vec![under_b]);
    assert!(pinned.pending.is_empty());
}

/// The consent predicate: a Profile is Cloud-syncing exactly when the
/// projection places a Cloud-enabled Workspace under it. An empty Cloud
/// Profile is not in the set (shown as not syncing, not absent — the list
/// is the account's Profiles, which this never filters), and a Workspace
/// moved out closes its former Profile.
#[test]
fn cloud_syncing_profiles_follow_placement() {
    let (a, b, empty) = (ProfileId::new(), ProfileId::new(), ProfileId::new());
    let w = WorkspaceId::new();
    let mut catalog = Catalog::default();
    for profile in [a, b, empty] {
        catalog.profile(profile, 1);
    }
    catalog.workspace(w, Some(a), 1);
    let candidates = [candidate(a, w)];
    assert_eq!(
        cloud_syncing_profiles(&catalog.projection, &candidates),
        BTreeSet::from([a])
    );
    let moved = catalog.admit_move(w, Some(b), 2);
    catalog.project(w, &moved, 2);
    assert_eq!(
        cloud_syncing_profiles(&catalog.projection, &candidates),
        BTreeSet::from([b])
    );
    // A Workspace that is not Cloud-enabled (no candidate) counts for nothing.
    assert!(cloud_syncing_profiles(&catalog.projection, &[]).is_empty());
}

/// **The M3 incident, as the core rules see it** (ROADMAP.final M3 exit):
/// a Workspace is moved offline to another Profile, the former Profile is
/// deleted, and the account reconnects while an MCP connection works in the
/// Workspace. The Profile-granted connection loses the Workspace with the
/// commit and never through the hint; the later-flag connection follows it
/// into the new Profile; the UUID-granted connection keeps it throughout;
/// deleting the former Profile grants nothing and breaks nothing. The
/// content half — same document ids, histories and blobs across the move —
/// is `koru-mcp-server`'s in-memory composition test.
#[test]
fn the_incident_converges_without_repair() {
    let (former, next) = (ProfileId::new(), ProfileId::new());
    let w = WorkspaceId::new();
    let mut catalog = Catalog::default();
    catalog.profile(former, 1);
    catalog.profile(next, 1);
    catalog.workspace(w, Some(former), 1);
    let candidates = [candidate(former, w)];
    let granted_former = request(vec![profile_grant(former, ProfileAccess::Allowed)], false);
    let follows_later = request(Vec::new(), true);
    let pinned = request(vec![workspace_grant(w, ProfileAccess::Allowed)], false);

    // Before: every connection reaches W through its own grant.
    let session = catalog.resolve(&candidates, &granted_former);
    assert_eq!(ids(&session), vec![w]);
    assert_eq!(ids(&catalog.resolve(&candidates, &follows_later)), vec![w]);
    assert_eq!(ids(&catalog.resolve(&candidates, &pinned)), vec![w]);

    // Offline: the device moves W to `next` and deletes `former`. On
    // reconnect the device publishes both units; the projection has read
    // neither yet.
    let moved = catalog.admit_move(w, Some(next), 2);
    let lagging = catalog.resolve(&candidates, &granted_former);
    assert!(
        lagging.workspaces.is_empty(),
        "no fallback to the former Profile"
    );
    assert_eq!(lagging.pending[0].workspace_id, w);
    assert!(
        !session
            .evaluation
            .is_current(&catalog.projection, &candidates, &catalog.admitted),
        "the cached session is fenced the moment a newer unit is admitted"
    );
    // The UUID grant is pending too — it waits for the projection, it does
    // not guess — and resumes with the commit.
    assert_eq!(catalog.resolve(&candidates, &pinned).pending.len(), 1);

    catalog.project(w, &moved, 2);
    catalog.delete_profile(former, 2);

    // After the commit, without any repair of ids or rows:
    assert!(
        ids(&catalog.resolve(&candidates, &granted_former)).is_empty(),
        "the former Profile's grant no longer reaches W"
    );
    assert_eq!(
        ids(&catalog.resolve(&candidates, &follows_later)),
        vec![w],
        "a connection that includes later Profiles follows W into `next`"
    );
    let pinned_after = catalog.resolve(&candidates, &pinned);
    assert_eq!(ids(&pinned_after), vec![w], "the UUID grant never let go");
    assert_eq!(pinned_after.workspaces[0].profile_id, next);
    assert_eq!(
        pinned_after.workspaces[0].workspace_incarnation_id, candidates[0].workspace_incarnation_id,
        "a move touches no content identity"
    );
    assert_eq!(
        cloud_syncing_profiles(&catalog.projection, &candidates),
        BTreeSet::from([next]),
        "the deleted Profile is not a Cloud Profile; the destination is"
    );
    // A grant on the deleted Profile itself yields nothing, even for a
    // Workspace whose placement still named it.
    let stuck = WorkspaceId::new();
    catalog.workspace(stuck, Some(former), 3);
    let stuck_candidates = [candidate(former, stuck)];
    assert!(ids(&catalog.resolve(&stuck_candidates, &granted_former)).is_empty());
    assert!(
        catalog
            .resolve(&stuck_candidates, &granted_former)
            .pending
            .is_empty()
    );
}
