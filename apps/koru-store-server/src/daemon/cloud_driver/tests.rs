// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_device::cloud::CloudId;
use koru_store_core::{
    CatalogUnitRef, Locale, ProfileAccountBinding, SetTransportMode, TransportMode, TransportPath,
    TransportSubject, service::EnsuredWorkspace,
};
use koru_sync_control::cloud::{
    CLOUD_PERMIT_VERSION_V2, CLOUD_WORKSPACE_DESCRIPTOR_VERSION_V2, CloudWorkspaceDescriptorV2,
};

use crate::daemon::memory::MemoryStore;

fn id(byte: u8) -> CloudId {
    let mut bytes = [byte; 16];
    // Canonical v4 shape, as every permit id must be.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    CloudId::from_bytes(bytes)
}

fn descriptor(placement: Option<CloudId>) -> CloudWorkspaceDescriptorV2 {
    CloudWorkspaceDescriptorV2 {
        version: CLOUD_WORKSPACE_DESCRIPTOR_VERSION_V2,
        workspace_id: id(1),
        workspace_incarnation_id: id(2),
        logical_replica_id: id(3),
        lineage_id: id(2),
        manifest_document_id: id(6),
        authorization_version: 1,
        membership_epoch: 1,
        cloud_access_epoch: 1,
        placement,
        placement_revision: 4,
    }
}

fn permit(scope: CloudPermitScopeV2) -> CloudPermitV2 {
    CloudPermitV2 {
        version: CLOUD_PERMIT_VERSION_V2,
        permit_id: uuid::Uuid::new_v4(),
        account_id: id(4),
        catalog_scope_id: Some(id(7)),
        scope,
        account_generation: 1,
        authority_generation: 1,
        expires_at_seconds: 600,
    }
}

fn workspace_permit(placement: Option<CloudId>) -> CloudPermitV2 {
    permit(CloudPermitScopeV2::Workspace {
        descriptor: descriptor(placement),
    })
}

fn profile_permit(units: Vec<ProfileUnitRef>) -> CloudPermitV2 {
    permit(CloudPermitScopeV2::ProfileUnits {
        dataset: CloudReplicaPrincipalV1::Profile {
            profile_id: id(5),
            lineage_id: id(7),
            logical_replica_id: id(8),
        },
        allowed_profile_units: units,
    })
}

/// A Workspace permit plans its Workspace under the descriptor's manifest,
/// with the placement carried as the scope's Profile hint — present, or nil
/// for an explicitly unassigned Workspace, which plans all the same
/// (root-catalog TECHNICAL.final §7.3: the former Profile is not a
/// prerequisite).
#[test]
fn a_workspace_permit_plans_its_workspace_under_the_descriptor_manifest() {
    let placed = workspace_permit(Some(id(5)));
    let (plan, scope) = plan_for(&placed).unwrap();
    assert_eq!(
        plan,
        Plan::Workspace {
            workspace_id: WorkspaceId::from_uuid(id(1).as_uuid()),
        }
    );
    assert_eq!(Some(scope.principal), placed.dataset());
    assert_eq!(scope.manifest, DocumentId::from_uuid(id(6).as_uuid()));
    assert_eq!(scope.profile, id(5));
    assert_eq!(
        scope.eligibility_workspace,
        WorkspaceId::from_uuid(id(1).as_uuid())
    );

    let unassigned = workspace_permit(None);
    let (plan, scope) = plan_for(&unassigned).unwrap();
    assert!(matches!(plan, Plan::Workspace { .. }));
    assert_eq!(scope.profile, CloudId::from_bytes([0; 16]));
}

/// The two catalog scopes have no dataset session and are never planned.
#[test]
fn a_catalog_scope_permit_is_refused() {
    assert_eq!(
        plan_for(&permit(CloudPermitScopeV2::UserCatalog))
            .map(|_| ())
            .unwrap_err(),
        "cloud_permit_scope"
    );
    assert_eq!(
        plan_for(&permit(CloudPermitScopeV2::UserPersonal {
            workspace_id: id(1)
        }))
        .map(|_| ())
        .unwrap_err(),
        "cloud_permit_scope"
    );
}

#[test]
fn a_profile_permit_plans_exactly_the_units_it_allows() {
    let profile = ProfileId::from_uuid(id(5).as_uuid());
    let (plan, _) = plan_for(&profile_permit(vec![ProfileUnitRef::Identity { profile }])).unwrap();
    assert_eq!(
        plan,
        Plan::Profile {
            profile_id: profile,
            private: false,
            personal_workspaces: Vec::new(),
        }
    );
    let (plan, scope) = plan_for(&profile_permit(vec![
        ProfileUnitRef::Identity { profile },
        ProfileUnitRef::Private { profile },
    ]))
    .unwrap();
    assert_eq!(
        plan,
        Plan::Profile {
            profile_id: profile,
            private: true,
            personal_workspaces: Vec::new(),
        }
    );
    assert_eq!(scope.manifest, DocumentId::from_uuid(uuid::Uuid::nil()));
    assert_eq!(scope.profile, id(5));

    // Personal is the Workspace-keyed user-personal scope in v2, and
    // another Profile's unit is not narrowed down: the whole permit is
    // refused either way.
    let workspace = WorkspaceId::from_uuid(id(1).as_uuid());
    assert_eq!(
        plan_for(&profile_permit(vec![ProfileUnitRef::Personal {
            owner: profile,
            workspace,
        }]))
        .map(|_| ())
        .unwrap_err(),
        "cloud_permit_profile_units"
    );
    let other = ProfileId::from_uuid(id(9).as_uuid());
    assert_eq!(
        plan_for(&profile_permit(vec![ProfileUnitRef::Identity {
            profile: other
        }]))
        .map(|_| ())
        .unwrap_err(),
        "cloud_permit_profile_units"
    );
}

// ------------------------------------------- the document half of coverage

fn document_unit(byte: u8) -> Unit {
    Unit::Document {
        workspace_id: id(1),
        document_id: id(byte),
    }
}

fn a_state(counter: i32) -> Vec<u8> {
    let mut version = loro::VersionVector::default();
    version.set_last(loro::ID::new(1, counter));
    version.encode()
}

/// **What coverage is computed against, and nothing else.**
///
/// A document the pass carried whole arrives with the exact state it was
/// carried at; one it deferred, refused or never read arrives as `None` —
/// required, and Cloud does not have it. A Profile unit is not a document and
/// contributes nothing at all.
#[test]
fn the_document_half_of_coverage_is_what_the_pass_carried() {
    let carried = document_unit(10);
    let held = document_unit(11);
    let unread = document_unit(12);
    let outcome = cloud_sync::Outcome {
        documents: vec![
            cloud_sync::DocumentPass {
                unit: carried,
                state: a_state(7),
                carried: true,
            },
            // Settled, but its upload was held: Cloud does not have this state.
            cloud_sync::DocumentPass {
                unit: held,
                state: a_state(8),
                carried: false,
            },
            // Carried, and the version read never happened. An empty state is
            // the empty version vector, which every state includes — reporting
            // it as covered would be claiming coverage out of a missing read.
            cloud_sync::DocumentPass {
                unit: unread,
                state: Vec::new(),
                carried: true,
            },
        ],
        ..cloud_sync::Outcome::default()
    };

    let documents = covered_documents(&outcome);
    assert_eq!(documents.len(), 3, "a required document was dropped");
    assert_eq!(
        documents
            .get(&DocumentId::from_uuid(id(10).as_uuid()))
            .expect("the carried document")
            .as_ref()
            .map(|state| state.as_bytes().to_vec()),
        Some(a_state(7)),
    );
    assert_eq!(
        documents.get(&DocumentId::from_uuid(id(11).as_uuid())),
        Some(&None),
        "a held upload was reported as covered"
    );
    assert_eq!(
        documents.get(&DocumentId::from_uuid(id(12).as_uuid())),
        Some(&None),
        "a document whose state was never read was reported as covered"
    );
}

/// A Profile dataset carries no documents, so it contributes no coverage
/// input: Identity and Private are facts about the dataset and about no
/// document at all.
#[test]
fn a_pass_that_carried_no_documents_contributes_no_coverage_input() {
    assert!(covered_documents(&cloud_sync::Outcome::default()).is_empty());
}

// ------------------------------------------- the v1 window's bootstrap

/// A permit of the **v1 window**: no `catalog_scope_id`, which is the one
/// thing that distinguishes it, because the v1 service has no catalog scope
/// to name while its v2 binding route still answers 404 (ROLLOUT.md §2).
fn v1_window_permit(placement: Option<CloudId>) -> CloudPermitV2 {
    CloudPermitV2 {
        catalog_scope_id: None,
        ..workspace_permit(placement)
    }
}

/// A fresh device: the account's Profile and nothing else, which is what a
/// Cloud bootstrap finds when the person adds this device to a workspace
/// they already have elsewhere.
fn fresh_device(profile: ProfileId) -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::open("fresh").unwrap());
    store
        .with_store(move |store| {
            store.provision_account_profile(
                profile,
                ProfileAccountBinding::from_identity_uuid(uuid::Uuid::new_v4()).unwrap(),
                "Person",
                Locale::default(),
                Timestamp::now(),
            )
        })
        .unwrap()
        .unwrap();
    admit_cloud(&store, profile);
    store
}

/// A second Profile on the same device — where the person had moved the
/// Workspace before this device joined.
fn second_profile(store: &MemoryStore) -> ProfileId {
    let profile = store
        .with_store(|store| {
            store.create_profile("Other", None, None, Locale::default(), Timestamp::now())
        })
        .unwrap()
        .unwrap()
        .id;
    admit_cloud(store, profile);
    profile
}

/// Raise a Profile to Cloud, as the account's host does when the person
/// turns Cloud sync on. Every subject is seeded Local-only, and under
/// Local-only root metadata is refused on both paths and a placement would
/// go to the private overlay instead of the durable unit — neither of which
/// is the state a Cloud-synced Workspace is bootstrapped into.
fn admit_cloud(store: &MemoryStore, profile: ProfileId) {
    store
        .with_store(move |store| {
            let subject = TransportSubject::Profile(profile);
            let expected_revision = store.transport_mode(subject).unwrap().revision;
            store
                .set_transport_mode(SetTransportMode {
                    subject,
                    expected_revision,
                    mode: TransportMode::Cloud,
                })
                .unwrap();
        })
        .unwrap();
}

fn descriptor_profile() -> ProfileId {
    ProfileId::from_uuid(id(5).as_uuid())
}

fn descriptor_workspace() -> (WorkspaceId, DocumentId) {
    (
        WorkspaceId::from_uuid(id(1).as_uuid()),
        DocumentId::from_uuid(id(6).as_uuid()),
    )
}

fn workspace_unit(workspace: WorkspaceId) -> CatalogUnitRef {
    CatalogUnitRef::Workspace { workspace }
}

/// **QUESTIONS.md Q7.** In the v1 window the bootstrap provisions the
/// replica's content and publishes **no** Workspace unit: the row, its owner
/// projection and the manifest routing land exactly as they did, while
/// `root_catalog_units` holds nothing for the Workspace and `root_placement`
/// answers `None`. The placement reads fall back to the projection, which is
/// the only evidence of where the Workspace is until a real unit arrives —
/// so the Workspace is listed under the descriptor's Profile and under no
/// other.
#[tokio::test]
async fn a_v1_window_bootstrap_provisions_the_row_and_publishes_no_unit() {
    let profile = descriptor_profile();
    let (workspace, manifest) = descriptor_workspace();
    let store = fresh_device(profile);
    let permit = v1_window_permit(Some(id(5)));
    assert_eq!(
        bootstrap_placement(&permit),
        ReplicaPlacement::HintWithoutUnit(profile),
        "the window places the row and nothing else"
    );

    ensure_workspace(
        &store.client(),
        workspace,
        manifest,
        bootstrap_placement(&permit),
    )
    .await
    .unwrap();

    let elsewhere = second_profile(&store);
    store
        .with_store(move |store| {
            let row = store.workspace_including_deleted(workspace).unwrap();
            assert_eq!(row.owner, profile, "the row is projected under the hint");
            assert_eq!(
                store
                    .replica_manifest_id_including_deleted(workspace)
                    .unwrap(),
                manifest,
                "the manifest document and its routing landed"
            );
            assert_eq!(
                store
                    .root_unit_frontier(workspace_unit(workspace), TransportPath::Cloud)
                    .unwrap(),
                None,
                "no Workspace unit was published"
            );
            assert_eq!(
                store.root_placement(workspace).unwrap(),
                None,
                "the placement is pending, not the stale hint"
            );
            assert_eq!(
                store.current_placement_profile(workspace).unwrap(),
                Some(profile),
                "the projection is the only evidence, and the reads use it"
            );
            assert!(
                !matches!(
                    store
                        .ensure_live_workspace(profile, "", Timestamp::now())
                        .unwrap(),
                    EnsuredWorkspace::Empty
                ),
                "placed_workspaces lists it under the descriptor's Profile"
            );
            assert!(
                matches!(
                    store
                        .ensure_live_workspace(elsewhere, "", Timestamp::now())
                        .unwrap(),
                    EnsuredWorkspace::Empty
                ),
                "and under no other Profile"
            );
        })
        .unwrap();
}

/// The point of publishing nothing: the first real unit from a linked device
/// is applied by the ordinary join — no `workspace_exists` conflict against
/// the row the bootstrap already wrote — and **is** the placement, even when
/// it names a Profile other than the descriptor's stale hint.
#[tokio::test]
async fn a_real_unit_after_a_v1_window_bootstrap_is_the_only_placement() {
    let profile = descriptor_profile();
    let (workspace, manifest) = descriptor_workspace();
    let store = fresh_device(profile);
    ensure_workspace(
        &store.client(),
        workspace,
        manifest,
        bootstrap_placement(&v1_window_permit(Some(id(5)))),
    )
    .await
    .unwrap();

    // Where the person had moved it before this device joined.
    let moved_to = second_profile(&store);
    seed_root_placement(&store.client(), moved_to, workspace)
        .await
        .expect("the real unit joins a unit-less row without a conflict");

    store
        .with_store(move |store| {
            assert_eq!(
                store
                    .root_placement(workspace)
                    .unwrap()
                    .and_then(|placement| placement.profile),
                Some(moved_to),
                "the real unit is the placement"
            );
            assert_eq!(
                store.current_placement_profile(workspace).unwrap(),
                Some(moved_to)
            );
            assert_eq!(
                store.workspace_including_deleted(workspace).unwrap().owner,
                moved_to,
                "and the projection followed it"
            );
            assert!(
                matches!(
                    store
                        .ensure_live_workspace(profile, "", Timestamp::now())
                        .unwrap(),
                    EnsuredWorkspace::Empty
                ),
                "the stale hint places nothing"
            );
        })
        .unwrap();
}

/// A move made on *this* device before any unit arrives creates the unit, as
/// a first move does for any Workspace that has none.
#[tokio::test]
async fn a_first_local_move_on_a_unit_less_workspace_creates_its_unit() {
    let profile = descriptor_profile();
    let (workspace, manifest) = descriptor_workspace();
    let store = fresh_device(profile);
    ensure_workspace(
        &store.client(),
        workspace,
        manifest,
        bootstrap_placement(&v1_window_permit(Some(id(5)))),
    )
    .await
    .unwrap();

    let to = second_profile(&store);
    store
        .with_store(move |store| {
            store.move_workspace(workspace, to).unwrap();
            assert_eq!(
                store
                    .root_placement(workspace)
                    .unwrap()
                    .and_then(|placement| placement.profile),
                Some(to),
                "the move created the unit and placed it"
            );
            assert!(
                store
                    .root_unit_frontier(workspace_unit(workspace), TransportPath::Cloud)
                    .unwrap()
                    .is_some(),
                "the unit is durable"
            );
            assert_eq!(
                store.workspace_including_deleted(workspace).unwrap().owner,
                to
            );
        })
        .unwrap();
}

/// Outside the window nothing changed. A v2 permit still bootstraps under
/// its **root unit only**: with no unit the provisioning is held pending and
/// writes nothing at all, and once the unit has arrived the replica is
/// provisioned under it — which is how a v2 Workspace gets its unit, since
/// root units arrive before content there.
#[tokio::test]
async fn a_v2_bootstrap_still_places_by_the_root_unit() {
    let profile = descriptor_profile();
    let (workspace, manifest) = descriptor_workspace();
    let store = fresh_device(profile);
    let permit = workspace_permit(Some(id(5)));
    assert_eq!(
        bootstrap_placement(&permit),
        ReplicaPlacement::KnownOnly,
        "a permit with a catalog scope is not of the window"
    );

    assert_eq!(
        ensure_workspace(
            &store.client(),
            workspace,
            manifest,
            bootstrap_placement(&permit),
        )
        .await
        .unwrap_err(),
        "placement_pending"
    );
    store
        .with_store(move |store| {
            assert!(store.workspace_including_deleted(workspace).is_err());
            assert_eq!(
                store
                    .root_unit_frontier(workspace_unit(workspace), TransportPath::Cloud)
                    .unwrap(),
                None
            );
        })
        .unwrap();

    seed_root_placement(&store.client(), profile, workspace)
        .await
        .unwrap();
    ensure_workspace(
        &store.client(),
        workspace,
        manifest,
        bootstrap_placement(&permit),
    )
    .await
    .unwrap();
    store
        .with_store(move |store| {
            assert_eq!(
                store
                    .root_placement(workspace)
                    .unwrap()
                    .and_then(|placement| placement.profile),
                Some(profile),
                "provisioned under the unit it joined"
            );
            assert!(
                store
                    .root_unit_frontier(workspace_unit(workspace), TransportPath::Cloud)
                    .unwrap()
                    .is_some()
            );
        })
        .unwrap();
}

/// And the peer path is untouched: a hinted provisioning still publishes the
/// Workspace's unit with its row, as Plan 2.1 built it. The no-unit case is
/// reachable only by naming `HintWithoutUnit`, which only the v1 window does.
#[test]
fn a_hinted_provisioning_still_publishes_its_unit() {
    let profile = descriptor_profile();
    let (workspace, manifest) = descriptor_workspace();
    let store = fresh_device(profile);
    store
        .with_store(move |store| {
            let record = store.profile(profile).unwrap();
            let row = koru_store_core::model::Workspace {
                id: workspace,
                owner: profile,
                name: String::new(),
                created_at: Timestamp::now(),
                last_opened_at: None,
                icon: None,
                color: None,
                pinned: false,
                archived: false,
                deleted_at: None,
                position: 0,
            };
            store
                .provision_replica(&record, &row, manifest, Timestamp::now())
                .unwrap();
            assert_eq!(
                store
                    .root_placement(workspace)
                    .unwrap()
                    .and_then(|placement| placement.profile),
                Some(profile),
                "the hint seeded the unit"
            );
            assert!(
                store
                    .root_unit_frontier(workspace_unit(workspace), TransportPath::Cloud)
                    .unwrap()
                    .is_some(),
                "and it is durable"
            );
        })
        .unwrap();
}
