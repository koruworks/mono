// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_device::cloud::CloudId;

#[test]
fn replacement_waits_for_ack_reuses_endpoint_and_never_extends_old_expiry() {
    let endpoint = ReplicationEndpointId::from_bytes([1; 32]);
    let id = CloudId::from_bytes([2; 16]);
    let principal = CloudReplicaPrincipalV1::Workspace {
        workspace_id: id,
        incarnation_id: id,
        logical_replica_id: id,
    };
    let mut state = CloudSessionHandover::new(endpoint, principal);
    state
        .acknowledge(
            endpoint,
            principal,
            SessionGeneration {
                generation: 1,
                expires_at_seconds: 600,
            },
            0,
        )
        .unwrap();
    assert!(!state.refresh_due(479));
    assert!(state.refresh_due(480));
    assert_eq!(state.active(599).unwrap().generation, 1);
    assert_eq!(state.active(600), Err("credential_expired"));
    state
        .acknowledge(
            endpoint,
            principal,
            SessionGeneration {
                generation: 2,
                expires_at_seconds: 1080,
            },
            580,
        )
        .unwrap();
    assert_eq!(state.active(580).unwrap().generation, 2);
    assert_eq!(state.draining(580).unwrap().generation, 1);
    assert_eq!(state.draining(580).unwrap().expires_at_seconds, 600);
    assert_eq!(state.draining(600), None);
    state.drained(1);
    assert_eq!(state.draining(600), None);
    assert_eq!(
        state.acknowledge(
            endpoint,
            principal,
            SessionGeneration {
                generation: 2,
                expires_at_seconds: 2000
            },
            590
        ),
        Err("handshake_replay")
    );
    state.fence();
    assert_eq!(state.active(590), Err("credential_expired"));
    for generation in [1, 2] {
        assert_eq!(
            state.acknowledge(
                endpoint,
                principal,
                SessionGeneration {
                    generation,
                    expires_at_seconds: 2000
                },
                590
            ),
            Err("handshake_replay")
        );
    }
}

#[test]
fn old_control_is_refused_before_any_state_change_and_peer_framing_is_unchanged() {
    use crate::*;
    assert_eq!(CONTROL_PROTOCOL_VERSION, 7);
    assert_eq!(CONTROL_FRAME_VERSION, 1);
    assert_eq!(PEER_PROTOCOL_VERSION, 5);
    let mut host = ControlHost::default();
    let old = ControlHello {
        protocol_version: 6,
        session_id: ControlSessionId::new(),
        local_device: DeviceId::new(),
    };
    assert!(matches!(host.begin_session(&old), Err(HostError::Protocol)));
    assert_eq!(host.status(), ControlStatus::default());
    assert!(host.session.is_none());
}

#[test]
fn expired_drain_does_not_block_next_acknowledgement() {
    let endpoint = ReplicationEndpointId::from_bytes([1; 32]);
    let id = CloudId::from_bytes([2; 16]);
    let principal = CloudReplicaPrincipalV1::Profile {
        profile_id: id,
        lineage_id: id,
        logical_replica_id: id,
    };
    let mut state = CloudSessionHandover::new(endpoint, principal);
    for (generation, expires_at_seconds, now) in [(1, 100, 0), (2, 200, 50), (3, 300, 101)] {
        state
            .acknowledge(
                endpoint,
                principal,
                SessionGeneration {
                    generation,
                    expires_at_seconds,
                },
                now,
            )
            .unwrap();
    }
    assert_eq!(state.active(101).unwrap().generation, 3);
    assert_eq!(state.draining(101).unwrap().generation, 2);
}

// ------------------------------------------------- v2 permits (Plan 3.1)

fn v4(n: u8) -> CloudId {
    let mut bytes = [n; 16];
    bytes[6] = 0x40 | (n & 0x0f);
    bytes[8] = 0x80 | (n & 0x3f);
    CloudId::from_bytes(bytes)
}

fn descriptor_v2(placement: Option<CloudId>) -> CloudWorkspaceDescriptorV2 {
    CloudWorkspaceDescriptorV2 {
        version: CLOUD_WORKSPACE_DESCRIPTOR_VERSION_V2,
        workspace_id: v4(1),
        workspace_incarnation_id: v4(2),
        logical_replica_id: v4(3),
        lineage_id: v4(2),
        manifest_document_id: v4(4),
        authorization_version: 1,
        membership_epoch: 1,
        cloud_access_epoch: 1,
        placement,
        placement_revision: 0,
    }
}

fn permit_v2(scope: CloudPermitScopeV2) -> CloudPermitV2 {
    CloudPermitV2 {
        version: CLOUD_PERMIT_VERSION_V2,
        permit_id: Uuid::new_v4(),
        account_id: v4(5),
        catalog_scope_id: v4(6),
        scope,
        account_generation: 1,
        authority_generation: 1,
        expires_at_seconds: 100,
    }
}

fn profile_id(id: CloudId) -> koru_store_core::ProfileId {
    koru_store_core::ProfileId::from_uuid(uuid::Uuid::from_bytes(*id.as_bytes()))
}

#[test]
fn v2_descriptor_places_without_a_profile_and_may_be_unassigned() {
    descriptor_v2(None).validate().unwrap();
    descriptor_v2(Some(v4(7))).validate().unwrap();
    assert_eq!(descriptor_v2(None).dataset().logical_replica_id(), v4(3));
    let mut not_v4 = descriptor_v2(Some(CloudId::from_bytes([9; 16])));
    assert_eq!(not_v4.validate(), Err(HostCloudError::DescriptorIdentity));
    not_v4.placement = None;
    not_v4.lineage_id = v4(3);
    assert_eq!(not_v4.validate(), Err(HostCloudError::DescriptorLineage));
    let mut negative = descriptor_v2(None);
    negative.placement_revision = -1;
    assert_eq!(negative.validate(), Err(HostCloudError::DescriptorEpoch));
    let mut v1 = descriptor_v2(None);
    v1.version = 1;
    assert_eq!(v1.validate(), Err(HostCloudError::DescriptorVersion));
    // The v1 descriptor's shape is untouched: it still needs its Profile.
    assert!(
        serde_json::from_str::<CloudWorkspaceDescriptorV1>(
            &serde_json::to_string(&descriptor_v2(None)).unwrap()
        )
        .is_err()
    );
}

#[test]
fn v2_permits_are_one_scope_each_and_need_no_profile_pair() {
    // A Workspace is admitted on its own descriptor: no Profile permit
    // beside it, no Profile in it.
    let workspace = permit_v2(CloudPermitScopeV2::Workspace {
        descriptor: descriptor_v2(None),
    });
    workspace.validate().unwrap();
    assert!(matches!(
        workspace.dataset(),
        Some(CloudReplicaPrincipalV1::Workspace { workspace_id, .. }) if workspace_id == v4(1)
    ));
    assert!(
        !serde_json::to_string(&workspace)
            .unwrap()
            .contains("profile_id")
    );

    let catalog = permit_v2(CloudPermitScopeV2::UserCatalog);
    catalog.validate().unwrap();
    assert_eq!(catalog.dataset(), None);
    let personal = permit_v2(CloudPermitScopeV2::UserPersonal {
        workspace_id: v4(1),
    });
    personal.validate().unwrap();
    assert_eq!(personal.dataset(), None);
    assert_eq!(
        permit_v2(CloudPermitScopeV2::UserPersonal {
            workspace_id: CloudId::from_bytes([1; 16])
        })
        .validate(),
        Err(HostCloudError::PermitIdentity)
    );

    let profile = v4(8);
    let dataset = CloudReplicaPrincipalV1::Profile {
        profile_id: profile,
        lineage_id: v4(9),
        logical_replica_id: v4(10),
    };
    let units = permit_v2(CloudPermitScopeV2::ProfileUnits {
        dataset,
        allowed_profile_units: vec![
            ProfileUnitRef::Identity {
                profile: profile_id(profile),
            },
            ProfileUnitRef::Private {
                profile: profile_id(profile),
            },
        ],
    });
    units.validate().unwrap();
    assert_eq!(units.dataset(), Some(dataset));
    // Personal is never a Profile unit here, another Profile's unit is
    // refused, the legacy private unit is refused, and an empty list is
    // not a permit.
    for allowed in [
        vec![ProfileUnitRef::Personal {
            owner: profile_id(profile),
            workspace: koru_store_core::WorkspaceId::from_uuid(uuid::Uuid::from_bytes(
                *v4(1).as_bytes(),
            )),
        }],
        vec![ProfileUnitRef::Identity {
            profile: profile_id(v4(11)),
        }],
        vec![ProfileUnitRef::LegacyPrivate {
            profile: profile_id(profile),
        }],
        Vec::new(),
        vec![
            ProfileUnitRef::Identity {
                profile: profile_id(profile),
            },
            ProfileUnitRef::Identity {
                profile: profile_id(profile),
            },
        ],
    ] {
        assert_eq!(
            permit_v2(CloudPermitScopeV2::ProfileUnits {
                dataset,
                allowed_profile_units: allowed,
            })
            .validate(),
            Err(HostCloudError::PermitProfileUnits)
        );
    }
    // A Profile-units scope over a Workspace dataset is a scope error.
    assert_eq!(
        permit_v2(CloudPermitScopeV2::ProfileUnits {
            dataset: descriptor_v2(None).dataset(),
            allowed_profile_units: vec![ProfileUnitRef::Identity {
                profile: profile_id(profile),
            }],
        })
        .validate(),
        Err(HostCloudError::PermitScope)
    );
    assert_eq!(HostCloudError::PermitScope.code(), "cloud_permit_scope");

    // Generations fence separately; a v1 number is refused.
    let mut stale = permit_v2(CloudPermitScopeV2::UserCatalog);
    stale.authority_generation = 0;
    assert_eq!(stale.validate(), Err(HostCloudError::PermitGeneration));
    let mut v1 = permit_v2(CloudPermitScopeV2::UserCatalog);
    v1.version = CLOUD_PERMIT_VERSION;
    assert_eq!(v1.validate(), Err(HostCloudError::PermitVersion));
    // Strict on the wire, and the scope is tagged.
    let json = serde_json::to_value(&units).unwrap();
    assert_eq!(json["scope"]["scope"], "profile_units");
    let mut extra = json;
    extra["profile_id"] = serde_json::json!("leak");
    assert!(serde_json::from_value::<CloudPermitV2>(extra).is_err());
}
