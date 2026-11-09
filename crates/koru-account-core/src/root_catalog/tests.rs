// SPDX-License-Identifier: MPL-2.0

//! Memory only: in-memory Loro units built by koru-store-core's own
//! constructors, no disk, no Store, no clock.

use std::collections::BTreeSet;

use koru_store_core::profile_units::{
    LegacyPersonalSource, PersonalManifest, migrate_personal_sources,
};
use koru_store_core::root_catalog::{
    CatalogUnitRef, DeletionRecord, ProfileCatalogUnit, WorkspaceCatalogUnit,
};
use koru_store_core::{DeletionId, ProfileId, WorkspaceId};
use uuid::Uuid;

use super::*;
use crate::workspace_lifecycle::WorkspaceCloudState;
use crate::{AuthIdentityId, CatalogScopeId, KoruIdentityId};

fn request(account: KoruIdentityId, declared: Option<DeclaredBinding>) -> BindingRequest {
    BindingRequest {
        account_id: account,
        authenticated_by: AuthIdentityId::mint(),
        declared,
        now_ms: 1_000,
    }
}

// ------------------------------------------------------------- binding

#[test]
fn concurrent_first_sign_ins_converge_on_one_binding() {
    let account = KoruIdentityId::mint();
    // Two devices read "no binding" at the same time and each decides to
    // create its own scope.
    let a = decide_binding(None, &request(account, None), CatalogScopeId::mint).unwrap();
    let b = decide_binding(None, &request(account, None), CatalogScopeId::mint).unwrap();
    assert!(matches!(a, BindingDecision::Create(_)));
    assert!(matches!(b, BindingDecision::Create(_)));
    assert_ne!(a.binding().catalog_scope_id, b.binding().catalog_scope_id);
    // Whichever insert commits first wins; the other reads it back. In
    // either order both devices end with the same binding.
    let mut first = BindingTable::new();
    let got_a = first.apply(a);
    let got_b = first.apply(b);
    assert_eq!(got_a, got_b);
    assert_eq!(got_a, a.binding());
    let mut second = BindingTable::new();
    let got_b = second.apply(b);
    let got_a = second.apply(a);
    assert_eq!(got_a, got_b);
    assert_eq!(got_b, b.binding());
    assert_eq!(got_a.binding_generation, 1);
}

#[test]
fn two_verified_identities_of_one_account_get_the_same_binding() {
    let account = KoruIdentityId::mint();
    let mut table = BindingTable::new();
    let google = BindingRequest {
        authenticated_by: AuthIdentityId::mint(),
        ..request(account, None)
    };
    let apple = BindingRequest {
        authenticated_by: AuthIdentityId::mint(),
        now_ms: 5_000,
        ..request(account, None)
    };
    let first = table.get_or_create(&google, CatalogScopeId::mint).unwrap();
    let second = table.get_or_create(&apple, CatalogScopeId::mint).unwrap();
    assert_eq!(first, second, "the identity is audit, never a key");
    assert_eq!(
        second.bound_at_ms, 1_000,
        "a later device never renames or restamps"
    );
    // A third device declaring what it already holds is idempotent.
    let third = table
        .get_or_create(
            &request(
                account,
                Some(DeclaredBinding {
                    account_id: account,
                    catalog_scope_id: first.catalog_scope_id,
                }),
            ),
            CatalogScopeId::mint,
        )
        .unwrap();
    assert_eq!(third, first);
}

#[test]
fn a_device_bound_to_account_a_refuses_account_b_and_never_merges() {
    let (a, b) = (KoruIdentityId::mint(), KoruIdentityId::mint());
    let mut table = BindingTable::new();
    let bound_a = table
        .get_or_create(&request(a, None), CatalogScopeId::mint)
        .unwrap();
    let bound_b = table
        .get_or_create(&request(b, None), CatalogScopeId::mint)
        .unwrap();
    assert_ne!(bound_a.catalog_scope_id, bound_b.catalog_scope_id);
    // The device holds A's binding and signs in to B.
    let refused = table
        .get_or_create(
            &request(
                b,
                Some(DeclaredBinding {
                    account_id: a,
                    catalog_scope_id: bound_a.catalog_scope_id,
                }),
            ),
            CatalogScopeId::mint,
        )
        .unwrap_err();
    assert_eq!(
        refused,
        BindingRefusal::AccountConflict {
            declared: a,
            session: b
        }
    );
    assert_eq!(refused.code(), "catalog_binding_conflict");
    // Nothing moved on either side.
    assert_eq!(table.get(a), Some(bound_a));
    assert_eq!(table.get(b), Some(bound_b));
    // A device declaring a scope the service never minted for this account
    // is refused too: a third device does not rename the binding.
    let renamed = table
        .get_or_create(
            &request(
                a,
                Some(DeclaredBinding {
                    account_id: a,
                    catalog_scope_id: CatalogScopeId::mint(),
                }),
            ),
            CatalogScopeId::mint,
        )
        .unwrap_err();
    assert!(matches!(
        renamed,
        BindingRefusal::ScopeMismatch { bound: Some(_), .. }
    ));
    let invented = decide_binding(
        None,
        &request(
            KoruIdentityId::mint(),
            Some(DeclaredBinding {
                account_id: KoruIdentityId::mint(),
                catalog_scope_id: CatalogScopeId::mint(),
            }),
        ),
        CatalogScopeId::mint,
    );
    assert!(matches!(
        invented,
        Err(BindingRefusal::AccountConflict { .. })
    ));
}

#[test]
fn a_corrupt_row_is_refused_not_answered() {
    let account = KoruIdentityId::mint();
    let foreign = AccountCatalogBinding {
        account_id: KoruIdentityId::mint(),
        catalog_scope_id: CatalogScopeId::mint(),
        bound_at_ms: 1,
        binding_generation: 1,
    };
    assert_eq!(
        decide_binding(Some(foreign), &request(account, None), CatalogScopeId::mint).unwrap_err(),
        BindingRefusal::Integrity
    );
}

// ----------------------------------------------------------- admission

struct Catalog {
    scope: CatalogScopeId,
    account_profiles: BTreeSet<ProfileId>,
    eligible_profiles: BTreeSet<ProfileId>,
    held_workspaces: BTreeSet<WorkspaceId>,
}

impl Catalog {
    fn new(account_profiles: &[ProfileId], eligible: &[ProfileId]) -> Self {
        Self {
            scope: CatalogScopeId::mint(),
            account_profiles: account_profiles.iter().copied().collect(),
            eligible_profiles: eligible.iter().copied().collect(),
            held_workspaces: BTreeSet::new(),
        }
    }
    fn ctx(&self) -> AdmissionContext<'_> {
        AdmissionContext {
            catalog_scope_id: self.scope,
            account_profiles: &self.account_profiles,
            eligible_profiles: &self.eligible_profiles,
            held_workspaces: &self.held_workspaces,
        }
    }
}

fn workspace_unit(
    workspace: WorkspaceId,
    placements: &[Option<ProfileId>],
) -> WorkspaceCatalogUnit {
    let unit = WorkspaceCatalogUnit::create(workspace, 7).unwrap();
    for placement in placements {
        unit.set_placement(*placement, Uuid::new_v4()).unwrap();
    }
    unit
}

fn stored(admitted: &Admitted) -> (&[u8], usize) {
    match admitted {
        Admitted::Stored {
            snapshot,
            redacted_ops,
            ..
        } => (snapshot, *redacted_ops),
        other => panic!("expected Stored, got {other:?}"),
    }
}

#[test]
fn a_workspace_unit_whose_history_names_a_hidden_profile_is_redacted_never_stored_raw() {
    let (visible, hidden) = (ProfileId::new(), ProfileId::new());
    let workspace = WorkspaceId::new();
    // Placed under the hidden (P2P-only) Profile first, then moved to the
    // Cloud-eligible one: the history names both.
    let unit = workspace_unit(workspace, &[Some(hidden), Some(visible)]);
    let raw = unit.snapshot().unwrap();
    let catalog = Catalog::new(&[visible, hidden], &[visible]);
    let admitted = admit_root_unit(
        &catalog.ctx(),
        CatalogUnitRef::Workspace { workspace },
        None,
        std::slice::from_ref(&raw),
    )
    .unwrap();
    let (snapshot, redacted) = stored(&admitted);
    assert_eq!(redacted, 1, "exactly the historical placement");
    assert_ne!(snapshot, raw.as_slice());
    let derived = WorkspaceCatalogUnit::from_snapshot(snapshot, workspace, 9).unwrap();
    assert_eq!(derived.placement().unwrap().profile, Some(visible));
    assert_eq!(
        derived.version(),
        unit.version(),
        "every operation id is kept"
    );
    // The hidden Profile's UUID is nowhere in the stored bytes, in either
    // spelling a Loro value could carry.
    let text = String::from_utf8_lossy(snapshot);
    assert!(!text.contains(&hidden.to_storage()));
    assert!(!text.contains(&hidden.as_uuid().simple().to_string()));
    assert!(text.contains(&visible.to_storage()));
}

#[test]
fn a_workspace_unit_currently_placed_under_a_hidden_profile_is_held() {
    let (visible, hidden) = (ProfileId::new(), ProfileId::new());
    let workspace = WorkspaceId::new();
    let unit = workspace_unit(workspace, &[Some(visible), Some(hidden)]);
    let catalog = Catalog::new(&[visible, hidden], &[visible]);
    let admitted = admit_root_unit(
        &catalog.ctx(),
        CatalogUnitRef::Workspace { workspace },
        None,
        &[unit.snapshot().unwrap()],
    )
    .unwrap();
    assert_eq!(
        admitted,
        Admitted::Held {
            unit: CloudUnitRef::WorkspaceCatalog {
                subject_id: workspace
            },
            reason: AdmissionHold::CurrentPlacementHidden,
        }
    );
    // Once the Profile's own unit has arrived, the same frames store.
    let catalog = Catalog::new(&[visible, hidden], &[visible, hidden]);
    let admitted = admit_root_unit(
        &catalog.ctx(),
        CatalogUnitRef::Workspace { workspace },
        None,
        &[unit.snapshot().unwrap()],
    )
    .unwrap();
    assert_eq!(stored(&admitted).1, 0);
}

#[test]
fn a_placement_naming_a_profile_the_account_does_not_hold_is_refused() {
    let (mine, foreign) = (ProfileId::new(), ProfileId::new());
    let workspace = WorkspaceId::new();
    let unit = workspace_unit(workspace, &[Some(foreign)]);
    let catalog = Catalog::new(&[mine], &[mine]);
    let refused = admit_root_unit(
        &catalog.ctx(),
        CatalogUnitRef::Workspace { workspace },
        None,
        &[unit.snapshot().unwrap()],
    )
    .unwrap_err();
    assert_eq!(
        refused,
        AdmissionRefusal::PlacementForeign { profile: foreign }
    );
    // An explicitly unassigned Workspace is admitted: null is not foreign.
    let unassigned = workspace_unit(workspace, &[None]);
    let admitted = admit_root_unit(
        &catalog.ctx(),
        CatalogUnitRef::Workspace { workspace },
        None,
        &[unassigned.snapshot().unwrap()],
    )
    .unwrap();
    assert!(matches!(admitted, Admitted::Stored { .. }));
    // A frame whose header names another subject is refused by the unit's
    // own validator.
    let other = WorkspaceId::new();
    let refused = admit_root_unit(
        &catalog.ctx(),
        CatalogUnitRef::Workspace { workspace: other },
        None,
        &[unassigned.snapshot().unwrap()],
    )
    .unwrap_err();
    assert!(matches!(refused, AdmissionRefusal::UnitInvalid { .. }));
}

#[test]
fn updates_against_a_held_unit_stage_pend_and_dedupe() {
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let unit = workspace_unit(workspace, &[Some(profile)]);
    let catalog = Catalog::new(&[profile], &[profile]);
    let first = admit_root_unit(
        &catalog.ctx(),
        CatalogUnitRef::Workspace { workspace },
        None,
        &[unit.snapshot().unwrap()],
    )
    .unwrap();
    let (held_snapshot, _) = stored(&first);
    let held = HeldUnit {
        snapshot: held_snapshot,
        revision: 1,
    };
    let before = unit.version();
    // A re-send of what Cloud holds changes nothing.
    let same = admit_root_unit(
        &catalog.ctx(),
        CatalogUnitRef::Workspace { workspace },
        Some(held),
        &[unit.snapshot().unwrap()],
    )
    .unwrap();
    assert!(matches!(same, Admitted::Unchanged { .. }));
    // A trash fact as an update since Cloud's frontier stores at revision 1.
    unit.record_deletion(DeletionId::new(), &DeletionRecord::new(10, None))
        .unwrap();
    let update = unit.updates_since(&before).unwrap();
    let next = admit_root_unit(
        &catalog.ctx(),
        CatalogUnitRef::Workspace { workspace },
        Some(held),
        &[update],
    )
    .unwrap();
    match &next {
        Admitted::Stored {
            expected_revision,
            frontier,
            ..
        } => {
            assert_eq!(*expected_revision, Some(1));
            assert_eq!(*frontier, unit.version().encode());
        }
        other => panic!("{other:?}"),
    }
    // An update whose ancestors Cloud lacks is pending at the held frontier.
    let after_first = unit.version();
    unit.record_deletion(DeletionId::new(), &DeletionRecord::new(11, None))
        .unwrap();
    let gap = unit.updates_since(&after_first).unwrap();
    let pending = admit_root_unit(
        &catalog.ctx(),
        CatalogUnitRef::Workspace { workspace },
        Some(held),
        &[gap],
    )
    .unwrap();
    assert_eq!(
        pending,
        Admitted::Pending {
            unit: CloudUnitRef::WorkspaceCatalog {
                subject_id: workspace
            },
            frontier: before.encode(),
        }
    );
}

#[test]
fn personal_provenance_is_redacted_and_requires_the_workspace_unit() {
    let (visible, hidden) = (ProfileId::new(), ProfileId::new());
    let workspace = WorkspaceId::new();
    // A v1 unit owned by the hidden Profile, migrated to v2 the way the
    // Store does it, so `meta.owner_profile_id` is history, not a relabel.
    let legacy = PersonalManifest::create(hidden, workspace).unwrap();
    legacy.set_peer_id(4_242).unwrap();
    let migrated = migrate_personal_sources(
        workspace,
        vec![LegacyPersonalSource {
            owner: hidden,
            workspace,
            snapshot: legacy.export_snapshot().unwrap(),
        }],
        77,
    )
    .unwrap();
    let raw = migrated.snapshot;
    let mut catalog = Catalog::new(&[visible, hidden], &[visible]);
    let refused = admit_personal_unit(&catalog.ctx(), workspace, None, std::slice::from_ref(&raw))
        .unwrap_err();
    assert_eq!(
        refused,
        AdmissionRefusal::WorkspaceUnitRequired { workspace }
    );
    catalog.held_workspaces.insert(workspace);
    let admitted =
        admit_personal_unit(&catalog.ctx(), workspace, None, std::slice::from_ref(&raw)).unwrap();
    let (snapshot, redacted) = stored(&admitted);
    assert_eq!(redacted, 1);
    let text = String::from_utf8_lossy(snapshot);
    assert!(!text.contains(&hidden.to_storage()));
    PersonalManifest::from_snapshot_v2(snapshot, workspace).unwrap();
    // Under a scope where that Profile is eligible, nothing is redacted.
    let catalog = Catalog {
        held_workspaces: [workspace].into_iter().collect(),
        ..Catalog::new(&[visible, hidden], &[visible, hidden])
    };
    let admitted = admit_personal_unit(&catalog.ctx(), workspace, None, &[raw]).unwrap();
    assert_eq!(stored(&admitted).1, 0);
}

#[test]
fn a_profile_unit_the_account_does_not_hold_is_refused() {
    let (mine, foreign) = (ProfileId::new(), ProfileId::new());
    let catalog = Catalog::new(&[mine], &[]);
    let unit = ProfileCatalogUnit::create(foreign, 3).unwrap();
    let refused = admit_root_unit(
        &catalog.ctx(),
        CatalogUnitRef::Profile { profile: foreign },
        None,
        &[unit.snapshot().unwrap()],
    )
    .unwrap_err();
    assert_eq!(
        refused,
        AdmissionRefusal::PlacementForeign { profile: foreign }
    );
    let unit = ProfileCatalogUnit::create(mine, 3).unwrap();
    let admitted = admit_root_unit(
        &catalog.ctx(),
        CatalogUnitRef::Profile { profile: mine },
        None,
        &[unit.snapshot().unwrap()],
    )
    .unwrap();
    assert!(matches!(
        admitted,
        Admitted::Stored {
            redacted_ops: 0,
            ..
        }
    ));
}

// ---------------------------------------------------------- projection

fn job(unit: CloudUnitRef, snapshot: &[u8], revision: i64) -> ProjectionJob<'_> {
    ProjectionJob {
        unit,
        snapshot,
        revision,
    }
}

#[test]
fn a_stale_job_never_overwrites_a_newer_placement_and_replay_is_idempotent() {
    let (a, b) = (ProfileId::new(), ProfileId::new());
    let workspace = WorkspaceId::new();
    let address = CloudUnitRef::WorkspaceCatalog {
        subject_id: workspace,
    };
    let unit = workspace_unit(workspace, &[Some(a)]);
    let under_a = unit.snapshot().unwrap();
    unit.set_placement(Some(b), Uuid::new_v4()).unwrap();
    let under_b = unit.snapshot().unwrap();

    let mut projection = DiscoveryProjection::new();
    assert_eq!(
        projection.apply(&job(address, &under_a, 1)).unwrap(),
        Applied::Applied
    );
    assert_eq!(
        projection.apply(&job(address, &under_b, 2)).unwrap(),
        Applied::Applied
    );
    let current = *projection.workspace(workspace).unwrap();
    assert_eq!(current.placement, Some(b));
    assert_eq!(current.placement_revision, 2);
    assert_eq!(current.watermark, 2);
    // The old job arrives late: stale, and the placement stands.
    assert_eq!(
        projection.apply(&job(address, &under_a, 1)).unwrap(),
        Applied::Stale { watermark: 2 }
    );
    assert_eq!(*projection.workspace(workspace).unwrap(), current);
    // The same job twice is a replay: nothing changes, the watermark does
    // not regress or advance.
    assert_eq!(
        projection.apply(&job(address, &under_b, 2)).unwrap(),
        Applied::Stale { watermark: 2 }
    );
    assert_eq!(*projection.workspace(workspace).unwrap(), current);
    // A newer revision with the same placement keeps placement_revision.
    unit.record_deletion(DeletionId::new(), &DeletionRecord::new(5, None))
        .unwrap();
    assert_eq!(
        projection
            .apply(&job(address, &unit.snapshot().unwrap(), 3))
            .unwrap(),
        Applied::Applied
    );
    let trashed = *projection.workspace(workspace).unwrap();
    assert_eq!(trashed.placement_revision, 2);
    assert_eq!(trashed.watermark, 3);
    assert_eq!(trashed.lifecycle, SubjectLifecycle::Deleted);
    // Reducing the jobs in any order gives the same projection.
    let shuffled = project([
        job(address, &unit.snapshot().unwrap(), 3),
        job(address, &under_a, 1),
        job(address, &under_b, 2),
    ])
    .unwrap();
    assert_eq!(shuffled.workspace(workspace).unwrap().placement, Some(b));
    assert_eq!(shuffled.workspace(workspace).unwrap().watermark, 3);
}

#[test]
fn coverage_names_exactly_the_units_the_projection_has_not_read() {
    let profile = ProfileId::new();
    let (w1, w2) = (WorkspaceId::new(), WorkspaceId::new());
    let p_unit = ProfileCatalogUnit::create(profile, 1).unwrap();
    let w1_unit = workspace_unit(w1, &[Some(profile)]);
    let w2_unit = workspace_unit(w2, &[Some(profile)]);
    let p = CloudUnitRef::ProfileCatalog {
        subject_id: profile,
    };
    let (a1, a2) = (
        CloudUnitRef::WorkspaceCatalog { subject_id: w1 },
        CloudUnitRef::WorkspaceCatalog { subject_id: w2 },
    );
    let admitted = [
        UnitFrontier {
            unit: p,
            revision: 1,
        },
        UnitFrontier {
            unit: a1,
            revision: 2,
        },
        UnitFrontier {
            unit: a2,
            revision: 1,
        },
    ];
    let mut projection = DiscoveryProjection::new();
    assert_eq!(
        projection.coverage(&admitted),
        Coverage::Partial {
            pending: vec![p, a1, a2]
        }
    );
    projection
        .apply(&job(p, &p_unit.snapshot().unwrap(), 1))
        .unwrap();
    projection
        .apply(&job(a1, &w1_unit.snapshot().unwrap(), 1))
        .unwrap();
    projection
        .apply(&job(a2, &w2_unit.snapshot().unwrap(), 1))
        .unwrap();
    // w1 was stored again at revision 2 and not yet projected: the receipt
    // says so instead of declaring the catalog covered.
    assert_eq!(
        projection.coverage(&admitted),
        Coverage::Partial { pending: vec![a1] }
    );
    assert_eq!(
        replay_needed(&projection, &admitted),
        vec![CatalogUnitRef::Workspace { workspace: w1 }]
    );
    projection
        .apply(&job(a1, &w1_unit.snapshot().unwrap(), 2))
        .unwrap();
    assert_eq!(projection.coverage(&admitted), Coverage::Complete);
    // Grouping by current placement counts each Workspace once.
    assert_eq!(projection.placed_under(Some(profile)).count(), 2);
    assert_eq!(projection.placed_under(None).count(), 0);
}

// ----------------------------------------------------------- placement

fn facts(account: KoruIdentityId, state: WorkspaceCloudState) -> HostedContentFacts {
    HostedContentFacts {
        workspace_id: WorkspaceId::new(),
        state,
        incarnation_id: Uuid::from_u128(9),
        logical_replica_id: Uuid::from_u128(10),
        membership: Some(MembershipFact {
            account_id: account,
            membership_epoch: 3,
            active: true,
        }),
        authorization_version: 4,
        cloud_access_epoch: 2,
    }
}

#[test]
fn content_admission_needs_membership_lifecycle_incarnation_and_policy_and_no_profile() {
    let account = KoruIdentityId::mint();
    let hosted = facts(account, WorkspaceCloudState::Enabled);
    let client = ClientContentPolicy {
        expected_incarnation_id: Some(Uuid::from_u128(9)),
        allows_cloud: true,
    };
    let admitted = decide_content_admission(account, &hosted, client).unwrap();
    assert_eq!(admitted.membership_epoch, 3);
    assert_eq!(admitted.incarnation_id, Uuid::from_u128(9));
    // Another account, a revoked membership, a disabled Workspace, a stale
    // incarnation and a closed policy are each refused with their own code.
    assert_eq!(
        decide_content_admission(KoruIdentityId::mint(), &hosted, client).unwrap_err(),
        ContentRefusal::WorkspaceNotAuthorized
    );
    let mut revoked = hosted;
    revoked.membership.as_mut().unwrap().active = false;
    assert_eq!(
        decide_content_admission(account, &revoked, client).unwrap_err(),
        ContentRefusal::WorkspaceNotAuthorized
    );
    assert_eq!(
        decide_content_admission(
            account,
            &facts(account, WorkspaceCloudState::Disabled),
            client
        )
        .unwrap_err(),
        ContentRefusal::CloudDisabled
    );
    assert_eq!(
        decide_content_admission(
            account,
            &facts(account, WorkspaceCloudState::Deleting),
            client
        )
        .unwrap_err(),
        ContentRefusal::CloudDeleting
    );
    assert_eq!(
        decide_content_admission(
            account,
            &hosted,
            ClientContentPolicy {
                expected_incarnation_id: Some(Uuid::from_u128(8)),
                ..client
            }
        )
        .unwrap_err(),
        ContentRefusal::CloudIncarnationStale
    );
    assert_eq!(
        decide_content_admission(
            account,
            &hosted,
            ClientContentPolicy {
                allows_cloud: false,
                ..client
            }
        )
        .unwrap_err(),
        ContentRefusal::PolicyRefusesCloud
    );
    // A fresh device with no incarnation yet is admitted and told the one
    // to bootstrap under.
    let fresh = decide_content_admission(
        account,
        &hosted,
        ClientContentPolicy {
            expected_incarnation_id: None,
            allows_cloud: true,
        },
    )
    .unwrap();
    assert_eq!(fresh.incarnation_id, hosted.incarnation_id);
}

#[test]
fn a_move_keeps_replica_incarnation_and_membership_and_moves_profile_unit_eligibility() {
    let (a, b) = (ProfileId::new(), ProfileId::new());
    let workspace = WorkspaceId::new();
    let owner = KoruIdentityId::mint();
    let content = WorkspaceContentIdentity {
        workspace_id: workspace,
        incarnation_id: Uuid::from_u128(1),
        logical_replica_id: Uuid::from_u128(2),
        membership_epoch: 5,
        members: [owner].into_iter().collect(),
    };
    let before = content.clone();
    let mut placement = WorkspacePlacement {
        placement: Some(a),
        placement_revision: 1,
    };
    assert_eq!(
        apply_move(&content, &mut placement, Some(b), 2),
        MoveOutcome::Moved {
            from: Some(a),
            to: Some(b)
        }
    );
    assert_eq!(placement.placement, Some(b));
    assert_eq!(placement.placement_revision, 2);
    assert_eq!(
        content, before,
        "a move touches nothing of the content identity"
    );
    assert_eq!(
        apply_move(&content, &mut placement, Some(b), 3),
        MoveOutcome::Unchanged
    );
    assert_eq!(placement.placement_revision, 2);

    // §4.4 through the projection: A holds the only Cloud Workspace; moving
    // it to B closes A's Identity/Private eligibility and opens B's.
    let a_unit = ProfileCatalogUnit::create(a, 1).unwrap();
    let b_unit = ProfileCatalogUnit::create(b, 2).unwrap();
    let w_unit = workspace_unit(workspace, &[Some(a)]);
    let cloud_enabled: BTreeSet<WorkspaceId> = [workspace].into_iter().collect();
    let mut projection = project([
        job(
            CloudUnitRef::ProfileCatalog { subject_id: a },
            &a_unit.snapshot().unwrap(),
            1,
        ),
        job(
            CloudUnitRef::ProfileCatalog { subject_id: b },
            &b_unit.snapshot().unwrap(),
            1,
        ),
        job(
            CloudUnitRef::WorkspaceCatalog {
                subject_id: workspace,
            },
            &w_unit.snapshot().unwrap(),
            1,
        ),
    ])
    .unwrap();
    assert!(profile_units_eligible(a, &projection, &cloud_enabled));
    assert!(!profile_units_eligible(b, &projection, &cloud_enabled));
    w_unit.set_placement(Some(b), Uuid::new_v4()).unwrap();
    projection
        .apply(&job(
            CloudUnitRef::WorkspaceCatalog {
                subject_id: workspace,
            },
            &w_unit.snapshot().unwrap(),
            2,
        ))
        .unwrap();
    assert!(!profile_units_eligible(a, &projection, &cloud_enabled));
    assert!(profile_units_eligible(b, &projection, &cloud_enabled));
    // A Workspace that is not Cloud-enabled opens nothing.
    assert!(!profile_units_eligible(b, &projection, &BTreeSet::new()));
}

#[test]
fn unit_refs_round_trip_as_kind_and_subject_and_nothing_else() {
    let workspace = WorkspaceId::new();
    let unit = CloudUnitRef::PersonalV2 {
        subject_id: workspace,
    };
    let json = serde_json::to_value(unit).unwrap();
    assert_eq!(json["kind"], "personal_v2");
    assert_eq!(json["subject_id"], workspace.to_storage());
    assert_eq!(json.as_object().unwrap().len(), 2);
    assert_eq!(serde_json::from_value::<CloudUnitRef>(json).unwrap(), unit);
    assert_eq!(unit.as_catalog(), None);
    assert_eq!(
        CloudUnitRef::catalog(CatalogUnitRef::Workspace { workspace }).as_catalog(),
        Some(CatalogUnitRef::Workspace { workspace })
    );
    assert!(unit.is_canonical());
    assert!(
        !CloudUnitRef::WorkspaceCatalog {
            subject_id: WorkspaceId::from_uuid(Uuid::from_u128(5))
        }
        .is_canonical()
    );
    assert!(
        serde_json::from_str::<CloudUnitRef>(
            r#"{"kind":"workspace_catalog","subject_id":"40000000-0000-4000-8000-000000000004","name":"x"}"#
        )
        .is_err()
    );
}
