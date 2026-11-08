// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeSet;

use koru_account_core::{KoruIdentityId, ProfileId, ProfileLifecycleRevision};
use koru_device::{DeviceDescriptorV1, DeviceId, DeviceSigningKey, ReplicationEndpointSecret};

use super::*;

#[test]
fn native_ownership_inspection_is_strict_bounded_in_schema_and_redacted() {
    let value = serde_json::json!({ "version": 1,
        "session_id": "00000000-0000-4000-8000-000000000002",
        "device_id": "00000000-0000-4000-8000-000000000005",
        "refresh_token": "inspection-secret" });
    let request: NativeSessionInspectRequestV1 = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        format!("{request:?}"),
        "NativeSessionInspectRequestV1(<redacted>)"
    );
    let mut extra = value.clone();
    extra["account_id"] = serde_json::json!("not-authority");
    assert!(serde_json::from_value::<NativeSessionInspectRequestV1>(extra).is_err());
    let mut wrong_version = value;
    wrong_version["version"] = serde_json::json!(2);
    assert!(serde_json::from_value::<NativeSessionInspectRequestV1>(wrong_version).is_err());
    let schema =
        serde_json::to_value(schemars::schema_for!(NativeSessionInspectRequestV1)).unwrap();
    assert_eq!(schema["properties"]["refresh_token"]["maxLength"], 43);
    assert_eq!(schema["properties"]["refresh_token"]["minLength"], 43);
    assert_eq!(schema["additionalProperties"], false);
}

#[test]
fn every_public_wire_type_has_a_schema() {
    let schemas = catalogue();
    assert_eq!(schemas.len(), TYPE_NAMES.len());
    for name in TYPE_NAMES {
        assert!(schemas.contains_key(name), "missing schema for {name}");
    }
}

#[test]
fn generated_schema_matches_the_checked_in_snapshot() {
    let value = serde_json::to_value(catalogue()).unwrap();
    if std::env::var_os("KORU_UPDATE_PUBLIC_API_SCHEMA").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../reference/schemas/koru-account-wire.schema.json");
        let text = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
        std::fs::write(path, text).unwrap();
    }
    let checked_in: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/schemas/koru-account-wire.schema.json"
    ))
    .unwrap();
    assert_eq!(
        checked_in, value,
        "run KORU_UPDATE_PUBLIC_API_SCHEMA=1 cargo test -p koru-account-wire generated_schema_matches_the_checked_in_snapshot"
    );
}

#[test]
fn generated_openapi_matches_the_checked_in_document() {
    let value = openapi();
    if std::env::var_os("KORU_UPDATE_PUBLIC_API_SCHEMA").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../reference/api/koru-account.openapi.json");
        let text = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
        std::fs::write(path, text).unwrap();
    }
    let checked_in: serde_json::Value = serde_json::from_str(include_str!(
        "../../../reference/api/koru-account.openapi.json"
    ))
    .unwrap();
    assert_eq!(
        checked_in, value,
        "run KORU_UPDATE_PUBLIC_API_SCHEMA=1 cargo test -p koru-account-wire generated_openapi_matches_the_checked_in_document"
    );
    let operations: usize = value["paths"]
        .as_object()
        .unwrap()
        .values()
        .map(|item| item.as_object().unwrap().len())
        .sum();
    assert_eq!(operations, ROUTES.len());
}

#[test]
fn security_sensitive_messages_reject_unknown_versions() {
    assert!(
        serde_json::from_str::<GoogleWebStartRequestV1>(
            r#"{"version":2,"return_target":"account","locale":"日本語"}"#
        )
        .is_err()
    );
    assert!(serde_json::from_str::<RefreshRequestV1>(
        r#"{"version":0,"session_id":"d9428888-122b-4f26-92b3-bc4f3ad56f52","refresh_token":"secret"}"#
    )
    .is_err());
    assert!(
        serde_json::from_str::<WebExchangeRequestV1>(r#"{"version":99,"grant":"grant"}"#).is_err()
    );
    assert!(serde_json::from_str::<SignedDeviceDescriptorV1>(
        r#"{"version":3,"device_id":"d9428888-122b-4f26-92b3-bc4f3ad56f52","display_name":"PC","signing_public_key":[],"endpoint_id":[],"created_at_millis":0,"endpoint_binding_signature":[]}"#
    )
    .is_err());
    assert!(serde_json::from_str::<DeviceAttestationRequestV1>(r#"{"version":2}"#).is_err());
    assert!(
        serde_json::from_str::<ProvisionDeviceRequestV1>(
            r#"{"version":2,"attestation":{"compact":"a.b.c"},"completion_receipt":"receipt"}"#
        )
        .is_err()
    );
}

#[test]
fn legacy_device_response_defaults_new_capabilities_and_truncation() {
    let old = r#"{
        "version":1,
        "device_id":"00000000-0000-4000-8000-000000000025",
        "display_name":"Legacy",
        "platform":"windows",
        "app_version":"0.0.3",
        "endpoint_id_hex":"00",
        "profile_states":[],
        "last_seen_at_millis":1,
        "revoked":false
    }"#;
    let device: DeviceSummaryV1 = serde_json::from_str(old).unwrap();
    assert_eq!(device.enrollment_protocol_version, None);
    assert_eq!(device.peer_protocol_version, None);
    let response: DeviceListResponseV1 = serde_json::from_value(serde_json::json!({
        "version": 1,
        "devices": [serde_json::from_str::<serde_json::Value>(old).unwrap()]
    }))
    .unwrap();
    assert!(!response.truncated);
}

#[test]
fn profile_names_and_locales_round_trip_unicode() {
    let profile = ProfileSummaryV1 {
        version: Version1,
        koru_identity_id: KoruIdentityId::mint(),
        profile_id: ProfileId::new(),
        name_snapshot: Some("仕事 👩🏽‍💻 — مرحبًا".into()),
        locale_snapshot: Some("zh-Hant-TW-u-nu-hanidec".into()),
        auth_identities: Vec::new(),
        lifecycle_revision: ProfileLifecycleRevision::INITIAL,
    };
    let json = serde_json::to_string(&profile).unwrap();
    let decoded: ProfileSummaryV1 = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, profile);
}

#[test]
fn profile_ids_and_koru_identity_ids_cannot_substitute() {
    fn profile(_: ProfileId) {}
    fn identity(_: KoruIdentityId) {}

    let profile_id = ProfileId::new();
    let identity_id = KoruIdentityId::mint();
    profile(profile_id);
    identity(identity_id);
    assert_ne!(profile_id.as_uuid(), identity_id.as_uuid());
}

#[test]
fn account_schema_has_no_canonical_document_content_shape() {
    let text = serde_json::to_string(&catalogue()).unwrap().to_lowercase();
    for forbidden in [
        "workspace_manifest",
        "document_body",
        "loro",
        "blob_bytes",
        "node_id",
    ] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} leaked into account wire"
        );
    }
    // The one document id the account wire may name is the Workspace
    // manifest's retained Store identity in the Cloud descriptor
    // (ADR-358): an identifier for bootstrap, never
    // a content shape. No other document id may appear.
    assert_eq!(
        text.matches("document_id").count(),
        text.matches("manifest_document_id").count(),
        "a document id other than the manifest identity leaked into account wire"
    );
    assert!(text.contains("manifest_document_id"));
}

#[test]
fn route_catalog_is_unique_and_schema_complete() {
    // Six of these are email sign-in's: start, verify, complete, sign in,
    // resend, and the native start. Five are app connections': consent's
    // read and decision, and the connections list, edit and revoke.
    assert_eq!(ROUTES.len(), 60);
    let mut ids = BTreeSet::new();
    let mut method_paths = BTreeSet::new();
    for route in ROUTES {
        assert!(ids.insert(route.id), "duplicate route id: {:?}", route.id);
        assert!(
            method_paths.insert((route.method.as_str(), route.path)),
            "duplicate route: {} {}",
            route.method.as_str(),
            route.path
        );
        let mut generator = schemars::generate::SchemaSettings::openapi3().into_generator();
        (route.request)(&mut generator);
        (route.response)(&mut generator);
        (route.error)(&mut generator);
        assert_eq!(route_by_id(route.id).path, route.path);
        assert!(
            route.path == "/v1/account"
                || route.path.starts_with("/v1/account/")
                || route.path.starts_with("/v1/oauth/")
        );
    }
}

#[test]
fn dynamic_routes_match_exact_nonempty_segment_cardinality_and_method() {
    let profile = "30000000-0000-4000-8000-000000000003";
    let device = "20000000-0000-4000-8000-000000000002";
    let candidates = format!("/v1/account/profiles/{profile}/enrollment-candidates");
    let attestations = format!("/v1/account/profiles/{profile}/device-attestations");
    let provisioning = format!("/v1/account/profiles/{profile}/devices/{device}/provisioning");
    assert_eq!(
        route_for(Method::Get, &candidates).map(|route| route.id),
        Some(RouteId::ListEnrollmentCandidates)
    );
    assert_eq!(
        route_for(Method::Post, &attestations).map(|route| route.id),
        Some(RouteId::IssueDeviceAttestation)
    );
    assert_eq!(
        route_for(Method::Put, &provisioning).map(|route| route.id),
        Some(RouteId::ProvisionDevice)
    );
    assert!(route_for(Method::Post, &provisioning).is_none());
    for invalid in [
        "/v1/account/profiles//enrollment-candidates".to_owned(),
        format!("{candidates}/extra"),
        format!("/v1/account/profiles/{profile}/devices//provisioning"),
        format!("{provisioning}/extra"),
    ] {
        assert!(!is_route_path(&invalid), "{invalid}");
    }
    assert!(route_for(Method::Get, "/v1/account/profiles/enrollment-candidates").is_none());
}

#[test]
fn developer_error_is_exactly_code_and_message() {
    let error = DeveloperErrorV1 {
        retry_after_millis: None,
        code: ErrorCode::parse(ErrorCode::FLOW_EXPIRED).unwrap(),
        message: "flow expired".into(),
    };
    let value = serde_json::to_value(error).unwrap();
    let fields: BTreeSet<_> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(fields, BTreeSet::from(["code", "message"]));
    assert!(
        serde_json::from_str::<DeveloperErrorV1>(
            r#"{"code":"flow_expired","message":"expired","detail":"secret"}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<DeveloperErrorV1>(
            r#"{"code":"Not Stable","message":"invalid code"}"#
        )
        .is_err()
    );
}

#[test]
fn existing_signed_device_descriptor_round_trips_and_verifies() {
    let signing = DeviceSigningKey::from_secret_bytes([7; 32]);
    let endpoint = ReplicationEndpointSecret::from_secret_bytes([9; 32]);
    let original = DeviceDescriptorV1::new(
        DeviceId::new(),
        "主电脑 🖥️".into(),
        &signing,
        endpoint.endpoint_id(),
        42,
    );
    let wire = SignedDeviceDescriptorV1::try_from(original.clone()).unwrap();
    let json = serde_json::to_string(&wire).unwrap();
    let decoded: SignedDeviceDescriptorV1 = serde_json::from_str(&json).unwrap();
    let restored = DeviceDescriptorV1::try_from(decoded).unwrap();
    assert_eq!(restored, original);
    restored.verify().unwrap();
}

#[test]
fn secrets_are_redacted_from_debug_output() {
    let sentinel = "secret-sentinel";
    for debug in [
        format!("{:?}", AuthorizationCode(sentinel.into())),
        format!("{:?}", OAuthState(sentinel.into())),
        format!("{:?}", PkceVerifier(sentinel.into())),
        format!("{:?}", OneUseGrant(sentinel.into())),
        format!("{:?}", AccessToken(sentinel.into())),
        format!("{:?}", RefreshToken(sentinel.into())),
        format!("{:?}", RefreshIdempotencyKey::from_bytes([7; 32])),
        format!("{:?}", WebSessionToken(sentinel.into())),
    ] {
        assert!(!debug.contains(sentinel));
        assert!(debug.contains("redacted"));
    }
}

#[test]
fn refresh_retry_key_is_exact_canonical_base64url() {
    let key = RefreshIdempotencyKey::from_bytes([0xa5; 32]);
    assert_eq!(key.as_str().len(), 43);
    assert_eq!(key.decode(), [0xa5; 32]);
    assert_eq!(RefreshIdempotencyKey::parse(key.as_str()).unwrap(), key);
    for invalid in [
        "",
        "AA",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA!",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        assert!(RefreshIdempotencyKey::parse(invalid).is_none(), "{invalid}");
        let json = format!("\"{invalid}\"");
        assert!(serde_json::from_str::<RefreshIdempotencyKey>(&json).is_err());
    }
}

#[test]
fn callback_outcome_carries_a_distinct_collision_flow() {
    let flow_id = "00000000-0000-4000-8000-000000000011".parse().unwrap();
    let outcome = GoogleCallbackResponseV1::CollisionRequired {
        version: Version1,
        collision_flow_id: flow_id,
        reauthentication: Box::new(AuthStartResponseV1 {
            version: Version1,
            flow_id,
            authorization_url: "https://accounts.example.test/auth".into(),
            expires_at_millis: 42,
        }),
        provider: koru_account_core::AuthProvider::Google,
        prompt: CollisionPromptV1::SelectAccount,
        web_return: None,
        native_return: None,
        reauthentication_web_return: None,
        reauthentication_native_return: None,
    };
    let json = serde_json::to_value(&outcome).unwrap();
    assert_eq!(json["outcome"], "collision_required");
    assert_eq!(json["collision_flow_id"], flow_id.to_string());
    assert_eq!(
        serde_json::from_value::<GoogleCallbackResponseV1>(json).unwrap(),
        outcome
    );
}

#[test]
fn authorization_and_nested_callback_debug_are_redacted() {
    let flow_id = "00000000-0000-4000-8000-000000000011".parse().unwrap();
    let start = AuthStartResponseV1 {
        version: Version1,
        flow_id,
        authorization_url: "https://accounts.example.test/auth?state=secret-sentinel".into(),
        expires_at_millis: 10,
    };
    let callback = GoogleCallbackResponseV1::CollisionRequired {
        version: Version1,
        collision_flow_id: flow_id,
        reauthentication: Box::new(start.clone()),
        provider: koru_account_core::AuthProvider::Google,
        prompt: CollisionPromptV1::SelectAccount,
        web_return: Some(WebReturnV1 {
            client_state: OAuthState("client-secret-sentinel".into()),
        }),
        native_return: None,
        reauthentication_web_return: Some(WebReturnV1 {
            client_state: OAuthState("phase-two-secret-sentinel".into()),
        }),
        reauthentication_native_return: None,
    };
    let debug = format!("{start:?} {callback:?}");
    assert!(!debug.contains("secret-sentinel"));
    assert!(debug.contains("<redacted>"));
}

#[test]
fn public_dto_debug_redacts_identifiers_pii_and_secrets() {
    let session_id = "00000000-0000-4000-8000-000000000021".parse().unwrap();
    let identity_id = "00000000-0000-4000-8000-000000000022".parse().unwrap();
    let auth_id = "00000000-0000-4000-8000-000000000023".parse().unwrap();
    let profile_id = ProfileId::parse("00000000-0000-4000-8000-000000000024").unwrap();
    let signing = DeviceSigningKey::from_secret_bytes([6; 32]);
    let endpoint = ReplicationEndpointSecret::from_secret_bytes([5; 32]);
    let descriptor = SignedDeviceDescriptorV1::try_from(DeviceDescriptorV1::new(
        DeviceId::from_uuid(uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000025").unwrap()),
        "device-name-sentinel".into(),
        &signing,
        endpoint.endpoint_id(),
        1,
    ))
    .unwrap();
    let account = AccountSummaryV1 {
        version: Version1,
        koru_identity_id: identity_id,
        display_name: "Steven".into(),
        auth_identities: vec![AuthIdentitySummaryV1 {
            version: Version1,
            auth_identity_id: auth_id,
            provider: koru_account_core::AuthProvider::Google,
            verified_email_snapshot: Some("person@example.invalid".into()),
        }],
        terms_version_at_creation: "terms-sentinel".into(),
        created_at_millis: 1,
    };
    let outputs = [
        format!(
            "{:?}",
            GoogleWebStartRequestV1 {
                version: Version1,
                return_target: SymbolicReturnTarget::Account,
                locale: "locale-sentinel".into(),
                client_state: OAuthState("state-sentinel".into())
            }
        ),
        format!(
            "{:?}",
            GoogleNativeStartRequestV1 {
                version: Version1,
                descriptor: descriptor.clone(),
                platform: koru_account_core::DevicePlatform::Windows,
                app_version: "app-sentinel".into(),
                callback_port: 1234,
                client_state: OAuthState("state-sentinel".into()),
                pkce_challenge: "pkce-sentinel".into(),
                locale: "locale-sentinel".into(),
                enrollment_protocol_version: Some(1),
                peer_protocol_version: Some(1),
                app_link_return: false,
            }
        ),
        format!("{account:?}"),
        format!(
            "{:?}",
            SessionSummaryV1 {
                version: Version1,
                session_id,
                koru_identity_id: identity_id,
                auth_identity_id: auth_id,
                device_id: Some(descriptor.device_id),
                expires_at_millis: 2,
                recent_auth_at_millis: 1
            }
        ),
        format!(
            "{:?}",
            RefreshRequestV1 {
                version: Version1,
                session_id,
                refresh_token: RefreshToken("refresh-sentinel".into()),
                idempotency_key: RefreshIdempotencyKey::from_bytes([4; 32])
            }
        ),
        format!(
            "{:?}",
            CreateProfileRequestV1 {
                version: Version1,
                profile_id,
                name_snapshot: "name-sentinel".into(),
                locale_snapshot: Some("locale-sentinel".into())
            }
        ),
        format!(
            "{:?}",
            ConfirmAddGoogleResponseV1 {
                version: Version1,
                koru_identity_id: identity_id,
                profile_id,
                status: ConfirmAddGoogleStatusV1::Added,
            }
        ),
        format!(
            "{:?}",
            RegisterDeviceRequestV1 {
                device_ownership_proof: None,
                version: Version1,
                descriptor: descriptor.clone(),
                platform: koru_account_core::DevicePlatform::Windows,
                app_version: "app-sentinel".into(),
                enrollment_protocol_version: Some(1),
                peer_protocol_version: Some(1)
            }
        ),
        format!(
            "{:?}",
            EnrollmentCandidateV1 {
                version: Version1,
                device_id: descriptor.device_id,
                signing_public_key: vec![91; 32],
                endpoint_id: vec![92; 32],
                platform: koru_account_core::DevicePlatform::Windows,
                app_version: "app-sentinel".into(),
                enrollment_protocol_version: 1,
                peer_protocol_version: 1,
            }
        ),
        format!(
            "{:?}",
            EnrollmentCandidatesResponseV1 {
                version: Version1,
                candidates: vec![EnrollmentCandidateV1 {
                    version: Version1,
                    device_id: descriptor.device_id,
                    signing_public_key: vec![91; 32],
                    endpoint_id: vec![92; 32],
                    platform: koru_account_core::DevicePlatform::Windows,
                    app_version: "app-sentinel".into(),
                    enrollment_protocol_version: 1,
                    peer_protocol_version: 1,
                }],
                truncated: false,
            }
        ),
        format!(
            "{:?}",
            DeviceAttestationResponseV1 {
                version: Version1,
                attestation: DeviceAttestationEvidenceV1 {
                    compact: "token-sentinel".into(),
                },
                expires_at_millis: 2,
            }
        ),
        format!(
            "{:?}",
            EnrollmentCompletionReceiptV1("receipt-sentinel".into())
        ),
        format!(
            "{:?}",
            ProvisionDeviceRequestV1 {
                version: Version1,
                attestation: DeviceAttestationEvidenceV1 {
                    compact: "token-sentinel".into(),
                },
                completion_receipt: EnrollmentCompletionReceiptV1("receipt-sentinel".into()),
            }
        ),
    ];
    for output in outputs {
        for forbidden in [
            "00000000-0000-4000-8000-000000000021",
            "00000000-0000-4000-8000-000000000022",
            "00000000-0000-4000-8000-000000000023",
            "00000000-0000-4000-8000-000000000024",
            "00000000-0000-4000-8000-000000000025",
            "person@example.invalid",
            "terms-sentinel",
            "device-name-sentinel",
            "locale-sentinel",
            "state-sentinel",
            "pkce-sentinel",
            "refresh-sentinel",
            "name-sentinel",
            "app-sentinel",
            "token-sentinel",
            "receipt-sentinel",
        ] {
            assert!(!output.contains(forbidden), "{output}");
        }
    }
}

#[test]
fn normal_manifests_exclude_transport_database_and_credential_dependencies() {
    let core = include_str!("../../koru-account-core/Cargo.toml");
    let wire = include_str!("../Cargo.toml");
    for forbidden in ["axum", "sqlx", "reqwest", "keyring", "openidconnect"] {
        assert!(!core.contains(forbidden), "core links {forbidden}");
        assert!(!wire.contains(forbidden), "wire links {forbidden}");
    }
}

#[test]
fn cloud_discovery_and_selection_routes_match_exact_paths() {
    let profile = "30000000-0000-4000-8000-000000000003";
    let selection = format!("/v1/account/profiles/{profile}/cloud/selection");
    assert_eq!(
        route_for(Method::Get, "/v1/account/cloud/workspaces").map(|route| route.id),
        Some(RouteId::ListCloudWorkspaces)
    );
    assert_eq!(
        route_for(Method::Put, &selection).map(|route| route.id),
        Some(RouteId::SelectProfileCloud)
    );
    assert!(route_for(Method::Post, "/v1/account/cloud/workspaces").is_none());
    assert!(route_for(Method::Post, &selection).is_none());
    for invalid in [
        "/v1/account/cloud/workspaces/extra".to_owned(),
        "/v1/account/profiles//cloud/selection".to_owned(),
        format!("{selection}/extra"),
    ] {
        assert!(!is_route_path(&invalid), "{invalid}");
    }
    let discovery = route_by_id(RouteId::ListCloudWorkspaces);
    assert_eq!(discovery.access, Access::NativeDeviceSession);
    assert_eq!(discovery.request_location, RequestLocation::None);
    let select = route_by_id(RouteId::SelectProfileCloud);
    assert_eq!(select.access, Access::NativeDeviceSession);
    assert_eq!(select.request_location, RequestLocation::Body);
}

#[test]
fn cloud_discovery_page_is_explicit_and_selection_request_is_strict() {
    let page = CloudWorkspacePageV1 {
        version: Version1,
        workspaces: vec![CloudWorkspaceCandidateV1 {
            kind: CloudWorkspaceCandidateKindV1::Cloud,
            profile_id: ProfileId::parse("30000000-0000-4000-8000-000000000003").unwrap(),
            workspace_id: koru_account_core::WorkspaceId::parse(
                "40000000-0000-4000-8000-000000000004",
            )
            .unwrap(),
            workspace_incarnation_id: uuid::Uuid::from_u128(9),
            authorization_version: 3,
            membership_epoch: 2,
            cloud_access_epoch: 1,
        }],
        next: None,
    };
    let value = serde_json::to_value(&page).unwrap();
    assert_eq!(value["next"], serde_json::Value::Null);
    assert_eq!(value["workspaces"][0]["kind"], "cloud");
    assert_eq!(
        value["workspaces"][0]["workspace_incarnation_id"],
        "00000000-0000-0000-0000-000000000009"
    );
    let decoded: CloudWorkspacePageV1 = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, page);
    let with_next = serde_json::json!({
        "version": 1,
        "workspaces": [],
        "next": {"profile_id": "30000000-0000-4000-8000-000000000003",
                 "workspace_id": "40000000-0000-4000-8000-000000000004"}
    });
    let decoded: CloudWorkspacePageV1 = serde_json::from_value(with_next).unwrap();
    assert!(decoded.next.is_some());
    assert!(
        serde_json::from_str::<CloudWorkspacePageV1>(
            r#"{"version":1,"workspaces":[],"next":null,"name":"leak"}"#
        )
        .is_err()
    );

    let request: CloudProfileSelectionRequestV1 = serde_json::from_str(
        r#"{"version":1,"workspace_id":"40000000-0000-4000-8000-000000000004"}"#,
    )
    .unwrap();
    assert_eq!(
        request.workspace_id.to_storage(),
        "40000000-0000-4000-8000-000000000004"
    );
    for invalid in [
        r#"{"version":2,"workspace_id":"40000000-0000-4000-8000-000000000004"}"#,
        r#"{"version":1,"workspace_id":"40000000-0000-1000-8000-000000000004"}"#,
        r#"{"version":1,"workspace_id":"40000000-0000-4000-8000-000000000004","profile_id":"x"}"#,
        r#"{"version":1,"workspace_id":"40000000-0000-4000-8000-000000000004","device_id":"x"}"#,
        r#"{"version":1,"workspace_id":"40000000-0000-4000-8000-000000000004".to_uppercase()}"#,
        r#"{"version":1}"#,
    ] {
        assert!(
            serde_json::from_str::<CloudProfileSelectionRequestV1>(invalid).is_err(),
            "{invalid}"
        );
    }
    assert!(
        serde_json::from_str::<CloudProfileSelectionRequestV1>(
            r#"{"version":1,"workspace_id":"40000000-0000-4000-8000-00000000000A"}"#
        )
        .is_err()
    );
    let response = CloudProfileSelectionResponseV1 {
        version: Version1,
        state: CloudProfileSelectionStateV1::Selected,
    };
    assert_eq!(
        serde_json::to_string(&response).unwrap(),
        r#"{"version":1,"state":"selected"}"#
    );
}

#[test]
fn cloud_descriptor_route_and_dto_are_strict() {
    let workspace = "40000000-0000-4000-8000-000000000004";
    let path = format!("/v1/account/workspaces/{workspace}/cloud/descriptor");
    assert_eq!(
        route_for(Method::Get, &path).map(|route| route.id),
        Some(RouteId::DescribeCloudWorkspace)
    );
    assert!(route_for(Method::Post, &path).is_none());
    assert!(!is_route_path("/v1/account/workspaces//cloud/descriptor"));
    let route = route_by_id(RouteId::DescribeCloudWorkspace);
    assert_eq!(route.access, Access::NativeDeviceSession);
    assert_eq!(route.request_location, RequestLocation::None);
    let descriptor = CloudWorkspaceDescriptorV1 {
        version: Version1,
        profile_id: ProfileId::parse("30000000-0000-4000-8000-000000000003").unwrap(),
        workspace_id: koru_account_core::WorkspaceId::parse(workspace).unwrap(),
        workspace_incarnation_id: uuid::Uuid::from_u128(9),
        logical_replica_id: uuid::Uuid::from_u128(10),
        lineage_id: uuid::Uuid::from_u128(9),
        manifest_document_id: uuid::Uuid::from_u128(11),
        authorization_version: 2,
        membership_epoch: 2,
        cloud_access_epoch: 1,
    };
    let value = serde_json::to_value(descriptor).unwrap();
    assert_eq!(
        value["manifest_document_id"],
        "00000000-0000-0000-0000-00000000000b"
    );
    let decoded: CloudWorkspaceDescriptorV1 = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, descriptor);
    assert!(
        serde_json::from_str::<CloudWorkspaceDescriptorV1>(
            r#"{"version":1,"profile_id":"30000000-0000-4000-8000-000000000003","workspace_id":"40000000-0000-4000-8000-000000000004","workspace_incarnation_id":"00000000-0000-0000-0000-000000000009","logical_replica_id":"00000000-0000-0000-0000-00000000000a","lineage_id":"00000000-0000-0000-0000-000000000009","manifest_document_id":"00000000-0000-0000-0000-00000000000b","authorization_version":2,"membership_epoch":2,"cloud_access_epoch":1,"workspace_name":"leak"}"#
        )
        .is_err()
    );
}
