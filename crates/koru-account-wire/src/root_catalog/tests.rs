// SPDX-License-Identifier: MPL-2.0

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use koru_account_core::root_catalog::SubjectLifecycle;
use koru_device::cloud::CloudId;
use serde_json::{Value, json};

use super::*;
use crate::{Access, Method, RequestLocation, RouteId, is_route_path, route_by_id, route_for};

const ACCOUNT: &str = "10000000-0000-4000-8000-00000000000a";
const SCOPE: &str = "20000000-0000-4000-8000-00000000000b";
const PROFILE: &str = "30000000-0000-4000-8000-00000000000c";
const WORKSPACE: &str = "40000000-0000-4000-8000-00000000000d";

fn account() -> KoruIdentityId {
    ACCOUNT.parse().unwrap()
}
fn scope() -> CatalogScopeId {
    SCOPE.parse().unwrap()
}
fn profile() -> ProfileId {
    ProfileId::parse(PROFILE).unwrap()
}
fn workspace() -> WorkspaceId {
    WorkspaceId::parse(WORKSPACE).unwrap()
}
fn cloud_id(value: &str) -> CloudId {
    CloudId::from_bytes(*uuid::Uuid::parse_str(value).unwrap().as_bytes())
}
fn credential() -> SyncCredential {
    let compact = format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(b"header"),
        URL_SAFE_NO_PAD.encode(b"claims"),
        URL_SAFE_NO_PAD.encode([7u8; 64])
    );
    SyncCredential::try_from(compact).unwrap()
}

fn round_trip<T>(value: &T) -> Value
where
    T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_value(value).unwrap();
    let back: T = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(&back, value);
    json
}

#[test]
fn the_binding_request_declares_only_what_the_device_holds_and_is_strict() {
    let fresh = CloudCatalogBindingRequestV2 {
        version: Version2,
        held: None,
    };
    assert_eq!(round_trip(&fresh), json!({"version": 2, "held": null}));
    let bound = CloudCatalogBindingRequestV2 {
        version: Version2,
        held: Some(CloudCatalogBindingRefV2 {
            account_id: account(),
            catalog_scope_id: scope(),
        }),
    };
    let json = round_trip(&bound);
    assert_eq!(json["held"]["account_id"], ACCOUNT);
    assert_eq!(json["held"]["catalog_scope_id"], SCOPE);
    for invalid in [
        // A v1 reader's version is refused on the number alone.
        json!({"version": 1, "held": null}),
        // The account never comes from the body.
        json!({"version": 2, "held": null, "account_id": ACCOUNT}),
        // Ids are canonical v4.
        json!({"version": 2, "held": {"account_id": ACCOUNT.to_uppercase(), "catalog_scope_id": SCOPE}}),
        json!({"version": 2, "held": {"account_id": ACCOUNT, "catalog_scope_id": "20000000-0000-1000-8000-000000000002"}}),
    ] {
        assert!(
            serde_json::from_value::<CloudCatalogBindingRequestV2>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
    // An absent `held` reads as nothing held, as every optional field of a
    // v1 request body does (`AccountCreationRequestV1::pkce_verifier`).
    assert_eq!(
        serde_json::from_value::<CloudCatalogBindingRequestV2>(json!({"version": 2})).unwrap(),
        fresh
    );
    let binding = CloudCatalogBindingV2 {
        version: Version2,
        account_id: account(),
        catalog_scope_id: scope(),
        bound_at_ms: 1_000,
        binding_generation: 1,
    };
    let json = round_trip(&binding);
    // The **set** of keys, sorted here rather than read in the order the
    // object happens to carry: `serde_json` keeps insertion order only with
    // its `preserve_order` feature, which this crate does not ask for but
    // workspace feature unification turns on, so the order differs between
    // `cargo test -p koru-account-wire` and `cargo test --workspace`. The
    // contract is which fields a binding answer carries, not their order.
    let mut keys = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "account_id",
            "binding_generation",
            "bound_at_ms",
            "catalog_scope_id",
            "version"
        ]
    );
}

#[test]
fn unit_refs_are_kind_and_canonical_subject_and_nothing_else() {
    for (unit, kind) in [
        (
            CloudUnitRefV2::ProfileCatalog {
                subject_id: profile(),
            },
            "profile_catalog",
        ),
        (
            CloudUnitRefV2::WorkspaceCatalog {
                subject_id: workspace(),
            },
            "workspace_catalog",
        ),
        (
            CloudUnitRefV2::PersonalV2 {
                subject_id: workspace(),
            },
            "personal_v2",
        ),
    ] {
        let json = round_trip(&unit);
        assert_eq!(json["kind"], kind);
        assert_eq!(json.as_object().unwrap().len(), 2);
        let core: koru_account_core::root_catalog::CloudUnitRef = unit.into();
        assert_eq!(CloudUnitRefV2::from(core), unit);
    }
    for invalid in [
        json!({"kind": "workspace_catalog", "subject_id": WORKSPACE.to_uppercase()}),
        json!({"kind": "workspace_catalog", "subject_id": "40000000-0000-1000-8000-000000000004"}),
        json!({"kind": "profile_catalog", "subject_id": PROFILE, "name": "leak"}),
        json!({"kind": "identity", "subject_id": PROFILE}),
        json!({"kind": "personal_v2", "subject_id": PROFILE, "profile_id": PROFILE}),
    ] {
        assert!(
            serde_json::from_value::<CloudUnitRefV2>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn the_join_bodies_carry_address_frontier_bytes_and_watermark_only() {
    let unit = CloudUnitRefV2::WorkspaceCatalog {
        subject_id: workspace(),
    };
    let catalog = CloudRootCatalogV2 {
        version: Version2,
        digest: vec![1, 2, 3],
        units: vec![CloudRootUnitStateV2 {
            unit,
            frontier: vec![0],
            projection_watermark: 4,
        }],
        next: None,
    };
    let json = round_trip(&catalog);
    assert_eq!(json["next"], Value::Null);
    assert_eq!(json["units"][0]["projection_watermark"], 4);
    let request = CloudRootUnitRequestV2 {
        version: Version2,
        unit,
        frontier: Vec::new(),
    };
    assert_eq!(round_trip(&request)["frontier"], json!([]));
    let updates = CloudRootUnitUpdatesV2 {
        version: Version2,
        unit,
        updates: vec![vec![9, 9]],
        projection_watermark: 0,
    };
    round_trip(&updates);
    for receipt in [
        CloudRootJoinReceiptV2::Applied {
            version: Version2,
            unit,
            frontier: vec![1],
            revision: 3,
            projection_watermark: 2,
            redacted_ops: 1,
        },
        CloudRootJoinReceiptV2::Pending {
            version: Version2,
            unit,
            frontier: vec![1],
        },
        CloudRootJoinReceiptV2::Unchanged {
            version: Version2,
            unit,
            frontier: vec![1],
        },
        CloudRootJoinReceiptV2::Held {
            version: Version2,
            unit,
            reason: CloudHoldReasonV2::CurrentPlacementHidden,
        },
    ] {
        let json = round_trip(&receipt);
        assert!(json["outcome"].is_string());
    }
    assert_eq!(
        serde_json::to_value(CloudHoldReasonV2::WorkspaceUnitRequired).unwrap(),
        "workspace_unit_required"
    );
    assert!(
        serde_json::from_value::<CloudRootJoinReceiptV2>(json!({
            "outcome": "applied", "version": 2,
            "unit": {"kind": "workspace_catalog", "subject_id": WORKSPACE},
            "frontier": [1], "revision": 3, "projection_watermark": 2, "redacted_ops": 0,
            "snapshot": [1, 2]
        }))
        .is_err(),
        "a receipt never carries bytes"
    );
}

#[test]
fn sync_scopes_are_one_at_a_time_and_the_response_dataset_matches_the_scope() {
    for (scope, tag) in [
        (CloudSyncScopeV2::UserCatalog, "user_catalog"),
        (
            CloudSyncScopeV2::Workspace {
                workspace_id: workspace(),
            },
            "workspace",
        ),
        (
            CloudSyncScopeV2::UserPersonal {
                workspace_id: workspace(),
            },
            "user_personal",
        ),
        (
            CloudSyncScopeV2::ProfileUnits {
                profile_id: profile(),
            },
            "profile_units",
        ),
    ] {
        let request = SyncTokenRequestV2 {
            version: Version2,
            scope,
        };
        let json = round_trip(&request);
        assert_eq!(json["scope"]["scope"], tag);
    }
    for invalid in [
        json!({"version": 1, "scope": {"scope": "user_catalog"}}),
        json!({"version": 2, "scope": {"scope": "workspace", "workspace_id": WORKSPACE, "profile_id": PROFILE}}),
        json!({"version": 2, "scope": {"scope": "workspace", "workspace_id": WORKSPACE.to_uppercase()}}),
        json!({"version": 2, "scope": {"scope": "user_catalog"}, "account_id": ACCOUNT}),
    ] {
        assert!(
            serde_json::from_value::<SyncTokenRequestV2>(invalid.clone()).is_err(),
            "{invalid}"
        );
    }

    let workspace_dataset = CloudReplicaPrincipalV1::Workspace {
        workspace_id: cloud_id(WORKSPACE),
        incarnation_id: cloud_id("50000000-0000-4000-8000-000000000005"),
        logical_replica_id: cloud_id("60000000-0000-4000-8000-000000000006"),
    };
    let route = CloudRouteResultV2::Unavailable {
        code: SyncUnavailableCode::SyncUnavailable,
        retry_after_seconds: 5,
    };
    let response = SyncTokenResponseV2 {
        version: Version2,
        credential: credential(),
        expires_at_seconds: 100,
        catalog_scope_id: scope(),
        scope: CloudSyncScopeV2::Workspace {
            workspace_id: workspace(),
        },
        authority_generation: 3,
        dataset: Some(workspace_dataset),
        route: route.clone(),
    };
    response.validate().unwrap();
    let json = round_trip(&response);
    assert_eq!(json["dataset"]["kind"], "workspace");
    assert_eq!(json["route"]["state"], "unavailable");
    assert!(!format!("{response:?}").contains(response.credential.compact()));
    // A catalog scope carries no dataset; a content scope must carry its own.
    let catalog = SyncTokenResponseV2 {
        scope: CloudSyncScopeV2::UserCatalog,
        dataset: None,
        ..response.clone()
    };
    catalog.validate().unwrap();
    assert_eq!(round_trip(&catalog)["dataset"], Value::Null);
    assert!(
        SyncTokenResponseV2 {
            scope: CloudSyncScopeV2::UserCatalog,
            ..response.clone()
        }
        .validate()
        .is_err()
    );
    assert!(
        SyncTokenResponseV2 {
            dataset: None,
            ..response.clone()
        }
        .validate()
        .is_err()
    );
    assert!(
        SyncTokenResponseV2 {
            scope: CloudSyncScopeV2::ProfileUnits {
                profile_id: profile()
            },
            ..response.clone()
        }
        .validate()
        .is_err(),
        "a Workspace dataset does not admit Profile units"
    );
    assert!(
        SyncTokenResponseV2 {
            authority_generation: 0,
            ..response.clone()
        }
        .validate()
        .is_err()
    );
    assert!(
        serde_json::from_value::<CloudRouteResultV2>(
            json!({"state": "unavailable", "code": "sync_unavailable", "retry_after_seconds": 5, "relay_url": "x"})
        )
        .is_err()
    );
}

#[test]
fn the_v2_descriptor_places_without_a_membership_profile_and_may_be_unassigned() {
    let descriptor = CloudWorkspaceDescriptorV2 {
        version: Version2,
        workspace_id: workspace(),
        workspace_incarnation_id: uuid::Uuid::from_u128(9),
        logical_replica_id: uuid::Uuid::from_u128(10),
        lineage_id: uuid::Uuid::from_u128(9),
        manifest_document_id: uuid::Uuid::from_u128(11),
        authorization_version: 2,
        membership_epoch: 2,
        cloud_access_epoch: 1,
        placement: WorkspacePlacement {
            placement: None,
            placement_revision: 4,
        },
    };
    let json = round_trip(&descriptor);
    assert_eq!(json["placement"]["placement"], Value::Null);
    assert!(json.get("profile_id").is_none());
    let placed = CloudWorkspaceDescriptorV2 {
        placement: WorkspacePlacement {
            placement: Some(profile()),
            placement_revision: 5,
        },
        ..descriptor
    };
    assert_eq!(round_trip(&placed)["placement"]["placement"], PROFILE);
    let mut leak = serde_json::to_value(descriptor).unwrap();
    leak["workspace_name"] = json!("leak");
    assert!(serde_json::from_value::<CloudWorkspaceDescriptorV2>(leak).is_err());
}

#[test]
fn the_discovery_receipt_groups_by_current_placement_and_says_its_coverage() {
    let receipt = CloudDiscoveryReceiptV2 {
        version: Version2,
        account_id: account(),
        catalog_scope_id: scope(),
        authorization_generation: 7,
        pass: 12,
        scope: CloudDiscoveryScopeV2::Workspaces,
        coverage: CloudCoverageV2::Partial {
            pending: vec![CloudUnitRefV2::ProfileCatalog {
                subject_id: profile(),
            }],
        },
        profiles: vec![CloudProfileResultV2 {
            profile_id: profile(),
            lifecycle: SubjectLifecycle::Live,
            profile_units_eligible: true,
            projection_watermark: 3,
        }],
        results: vec![CloudWorkspaceCandidateV2 {
            workspace_id: workspace(),
            workspace_incarnation_id: uuid::Uuid::from_u128(9),
            lifecycle: SubjectLifecycle::Live,
            membership: CloudMembershipV2 {
                authorization_version: 2,
                membership_epoch: 2,
                cloud_access_epoch: 1,
                documents_write: true,
            },
            placement: WorkspacePlacement {
                placement: Some(profile()),
                placement_revision: 3,
            },
            projection_watermark: 3,
        }],
        next: Some(CloudDiscoveryCursorV2 {
            workspace_id: workspace(),
        }),
    };
    let json = round_trip(&receipt);
    assert_eq!(json["scope"], "workspaces");
    assert_eq!(json["coverage"]["state"], "partial");
    assert_eq!(json["coverage"]["pending"][0]["kind"], "profile_catalog");
    assert_eq!(json["results"][0]["placement"]["placement"], PROFILE);
    assert_eq!(json["results"][0]["lifecycle"], "live");
    let text = json.to_string();
    assert!(!text.contains("name"));
    for coverage in [
        CloudCoverageV2::Complete,
        CloudCoverageV2::Held {
            reason: CloudHoldReasonV2::CurrentPlacementHidden,
        },
    ] {
        round_trip(&coverage);
    }
    // A state that names a payload needs it. (The unit `complete` ignores
    // siblings: serde's internally tagged unit variants accept any map once
    // the tag matches, whatever `deny_unknown_fields` says, as
    // `app_connections`' `pending` does; coverage is read, never sent.)
    for incomplete in [json!({"state": "partial"}), json!({"state": "held"})] {
        assert!(
            serde_json::from_value::<CloudCoverageV2>(incomplete.clone()).is_err(),
            "{incomplete}"
        );
    }
}

#[test]
fn the_v2_cloud_routes_resolve_by_exact_path_and_are_native_device_sessions() {
    let descriptor = format!("/v1/account/cloud/workspaces/{WORKSPACE}/descriptor");
    for (method, path, id, location) in [
        (
            Method::Put,
            "/v1/account/cloud/catalog/binding",
            RouteId::BindAccountCatalog,
            RequestLocation::Body,
        ),
        (
            Method::Get,
            "/v1/account/cloud/catalog",
            RouteId::ReadCloudRootCatalog,
            RequestLocation::None,
        ),
        (
            Method::Post,
            "/v1/account/cloud/catalog/units/pull",
            RouteId::PullCloudRootUnit,
            RequestLocation::Body,
        ),
        (
            Method::Post,
            "/v1/account/cloud/catalog/units/push",
            RouteId::PushCloudRootUnit,
            RequestLocation::Body,
        ),
        (
            Method::Post,
            "/v1/account/cloud/sync-token",
            RouteId::IssueCloudSyncTokenV2,
            RequestLocation::Body,
        ),
        (
            Method::Get,
            descriptor.as_str(),
            RouteId::DescribeCloudWorkspaceV2,
            RequestLocation::None,
        ),
        (
            Method::Get,
            "/v1/account/cloud/discovery",
            RouteId::DiscoverCloudCatalog,
            RequestLocation::None,
        ),
    ] {
        assert_eq!(
            route_for(method, path).map(|route| route.id),
            Some(id),
            "{path}"
        );
        let route = route_by_id(id);
        assert_eq!(route.access, Access::NativeDeviceSession);
        assert_eq!(route.request_location, location);
        assert!(route.id.operation_id().starts_with("account.cloud_"));
    }
    // The v1 routes keep their paths and meanings beside the v2 ones.
    assert_eq!(
        route_for(Method::Get, "/v1/account/cloud/workspaces").map(|route| route.id),
        Some(RouteId::ListCloudWorkspaces)
    );
    assert!(route_for(Method::Get, "/v1/account/cloud/sync-token").is_none());
    assert!(!is_route_path("/v1/account/cloud/workspaces//descriptor"));
    assert!(!is_route_path("/v1/account/cloud/catalog/units"));
}

#[test]
fn v2_schemas_are_in_the_catalogue_and_name_no_content() {
    let schemas = crate::catalogue();
    for name in [
        "Version2",
        "CloudCatalogBindingV2",
        "CloudUnitRefV2",
        "CloudRootJoinReceiptV2",
        "SyncTokenRequestV2",
        "SyncTokenResponseV2",
        "CloudWorkspaceDescriptorV2",
        "CloudDiscoveryReceiptV2",
    ] {
        assert!(schemas.contains_key(name), "{name}");
    }
    let response = serde_json::to_value(&schemas["SyncTokenResponseV2"]).unwrap();
    assert_eq!(response["properties"]["version"]["const"], 2);
    assert_eq!(response["additionalProperties"], false);
    let text = serde_json::to_string(&schemas["CloudDiscoveryReceiptV2"])
        .unwrap()
        .to_lowercase();
    assert!(!text.contains("\"name\""));
}
