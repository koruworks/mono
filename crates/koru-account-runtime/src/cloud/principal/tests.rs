// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_account_core::{KoruIdentityId, ProfileId, WorkspaceId};
use koru_account_wire::{CloudWorkspaceDescriptorV1, Version1};
use koru_device::ReplicationEndpointSecret;
use koru_store_core::profile_units::ProfileUnitRef;

fn v4(byte: u8) -> Uuid {
    let mut bytes = [byte; 16];
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn account() -> KoruIdentityId {
    v4(1).to_string().parse().unwrap()
}

fn endpoint(byte: u8) -> ReplicationEndpointId {
    ReplicationEndpointId::from_bytes(
        ed25519_dalek::SigningKey::from_bytes(&[byte; 32])
            .verifying_key()
            .to_bytes(),
    )
}

fn permit() -> CloudPermitV1 {
    CloudPermitV1 {
        version: 1,
        permit_id: v4(9),
        dataset: koru_device::cloud::CloudReplicaPrincipalV1::Workspace {
            workspace_id: cloud(v4(3)),
            incarnation_id: cloud(v4(4)),
            logical_replica_id: cloud(v4(5)),
        },
        account_id: account(),
        profile_id: ProfileId::from_uuid(v4(2)),
        authority_workspace_id: cloud(v4(3)),
        authority_workspace_incarnation_id: cloud(v4(4)),
        account_generation: 1,
        expires_at_seconds: 4_000,
        allowed_profile_units: Vec::new(),
        workspace_descriptor: Some(CloudWorkspaceDescriptorV1 {
            version: Version1,
            profile_id: ProfileId::from_uuid(v4(2)),
            workspace_id: WorkspaceId::from_uuid(v4(3)),
            workspace_incarnation_id: v4(4),
            logical_replica_id: v4(5),
            lineage_id: v4(4),
            manifest_document_id: v4(6),
            authorization_version: 4,
            membership_epoch: 5,
            cloud_access_epoch: 6,
        }),
    }
}

fn claims() -> SyncCredentialClaimsV1 {
    let body = serde_json::json!({
        "ver": 1,
        "purpose": "koru-sync",
        "iss": "https://us.example.invalid",
        "aud": "koru-cloud-sync",
        "region": "US",
        "sub": account().to_string(),
        "profile_id": v4(2).to_string(),
        "device_id": v4(7).to_string(),
        "client_endpoint_id": endpoint(1).as_bytes().to_vec(),
        "workspace_id": v4(3).to_string(),
        "workspace_incarnation_id": v4(4).to_string(),
        "workspace_replica_id": v4(5).to_string(),
        "profile_lineage_id": v4(4).to_string(),
        "profile_replica_id": v4(8).to_string(),
        "authorization_version": 4,
        "membership_epoch": 5,
        "cloud_access_epoch": 6,
        "iat": 1_000,
        "exp": 4_000,
        "jti": v4(10).to_string(),
    });
    serde_json::from_value(body).unwrap()
}

fn compact(claims: &SyncCredentialClaimsV1) -> SyncCredential {
    let head = URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA","typ":"koru-sync-credential+jwt"}"#);
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
    let signature = URL_SAFE_NO_PAD.encode([9_u8; 64]);
    SyncCredential::try_from(format!("{head}.{body}.{signature}")).unwrap()
}

#[test]
fn the_verified_credential_payload_decodes_to_its_claims() {
    let claims = claims();
    let decoded = credential_claims(&compact(&claims)).unwrap();
    assert_eq!(decoded.jti, claims.jti);
    assert_eq!(decoded.sub, account());
    assert_eq!(decoded.client_endpoint_id, endpoint(1));
}

#[test]
fn a_principal_is_formed_from_the_permit_and_the_claims_and_cross_checked() {
    let principal = build_principal(
        &permit(),
        &claims(),
        endpoint(1),
        endpoint(2),
        v4(11),
        Accepted {
            session_generation: 3,
            expires_at_seconds: 4_000,
        },
    )
    .unwrap();
    assert_eq!(principal.permit_id, v4(9));
    assert_eq!(principal.account_id, cloud(v4(1)));
    assert_eq!(principal.profile_id, cloud(v4(2)));
    assert_eq!(principal.credential_id, cloud(v4(10)));
    assert_eq!(principal.worker_endpoint_id, endpoint(2));
    assert_eq!(principal.client_endpoint_id, endpoint(1));
    assert_eq!(principal.session_generation, 3);
    assert_eq!(
        (
            principal.authorization_version,
            principal.membership_epoch,
            principal.cloud_access_epoch
        ),
        (4, 5, 6)
    );
    assert_eq!(principal.control_session_id, v4(11));
    assert!(principal.validate().is_ok());

    // A credential for another endpoint, another Workspace or another
    // account is not this permit's credential.
    let wrong_endpoint = build_principal(
        &permit(),
        &claims(),
        endpoint(3),
        endpoint(2),
        v4(11),
        Accepted {
            session_generation: 3,
            expires_at_seconds: 4_000,
        },
    );
    assert_eq!(wrong_endpoint.unwrap_err(), Failure::DatasetMismatch);
    let mut other_workspace = permit();
    other_workspace.authority_workspace_id = cloud(v4(12));
    assert_eq!(
        build_principal(
            &other_workspace,
            &claims(),
            endpoint(1),
            endpoint(2),
            v4(11),
            Accepted {
                session_generation: 3,
                expires_at_seconds: 4_000,
            },
        )
        .unwrap_err(),
        Failure::DatasetMismatch
    );
}

#[test]
fn the_control_permit_carries_every_field_as_a_canonical_id() {
    let runtime = permit();
    let control = control_permit(&runtime);
    assert_eq!(control.permit_id, runtime.permit_id);
    assert_eq!(control.account_id, cloud(v4(1)));
    assert_eq!(control.profile_id, cloud(v4(2)));
    assert_eq!(control.dataset, runtime.dataset);
    assert_eq!(control.allowed_profile_units, runtime.allowed_profile_units);
    let descriptor = control.workspace_descriptor.unwrap();
    assert_eq!(descriptor.manifest_document_id, cloud(v4(6)));
    assert_eq!(descriptor.lineage_id, descriptor.workspace_incarnation_id);
    assert!(descriptor.validate().is_ok());
    assert!(control.validate().is_ok());

    // A Profile permit carries its allowed units through unchanged and
    // validates without a descriptor.
    let mut profile = permit();
    profile.dataset = koru_device::cloud::CloudReplicaPrincipalV1::Profile {
        profile_id: cloud(v4(2)),
        lineage_id: cloud(v4(4)),
        logical_replica_id: cloud(v4(8)),
    };
    profile.workspace_descriptor = None;
    profile.allowed_profile_units = vec![ProfileUnitRef::Identity {
        profile: ProfileId::from_uuid(v4(2)),
    }];
    let control = control_permit(&profile);
    assert_eq!(control.allowed_profile_units, profile.allowed_profile_units);
    assert!(control.workspace_descriptor.is_none());
    assert!(control.validate().is_ok());
}

#[test]
fn stages_map_one_to_one_and_unknown_codes_collapse_to_one_static() {
    for (control, runtime) in [
        (CloudStage::AwaitingAuthority, Stage::AwaitingAuthority),
        (CloudStage::Connecting, Stage::Connecting),
        (CloudStage::Reconciling, Stage::Reconciling),
        (CloudStage::WaitingForConsent, Stage::WaitingForConsent),
        (CloudStage::Offline, Stage::Offline),
        (CloudStage::Unavailable, Stage::Unavailable),
        (CloudStage::CheckingCoverage, Stage::CheckingCoverage),
        (CloudStage::Settled, Stage::Settled),
    ] {
        assert_eq!(Stage::from(control), runtime);
    }
    assert_eq!(static_code("replica_fenced"), "replica_fenced");
    assert_eq!(
        static_code("credential_expired"),
        crate::cloud::CREDENTIAL_EXPIRED
    );
    // A pass that finished with one unit left behind survives to the report
    // with its own word, rather than collapsing into `cloud_session_failed`
    // beside the codes that end a session.
    assert_eq!(
        static_code("cloud_unit_unavailable"),
        "cloud_unit_unavailable"
    );
    // An app connection's creation refusal keeps its word (Cloud MCP Plan
    // 2.3): the hosted server reads it as the creation refusal, not as a
    // generic failure.
    assert_eq!(static_code("sync_unit_exists"), "sync_unit_exists");
    assert_eq!(static_code("something_new"), "cloud_session_failed");
    let _ = ReplicationEndpointSecret::generate();
}
