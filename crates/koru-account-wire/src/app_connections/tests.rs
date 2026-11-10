// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeMap;

use koru_account_core::app_connections::{
    GrantEvaluation, GrantPending, PendingReason, ProjectedPlacement, WorkspaceRevision,
};
use serde_json::{Value, json};

use super::*;

const PROFILE: &str = "30000000-0000-4000-8000-000000000003";
const WORKSPACE: &str = "40000000-0000-4000-8000-000000000004";

fn profile() -> ProfileId {
    ProfileId::parse(PROFILE).unwrap()
}

fn workspace() -> WorkspaceId {
    WorkspaceId::parse(WORKSPACE).unwrap()
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

/// **Additive, v1 shapes keep working**: a consent, a connection update and
/// a summary without the Workspace grants parse as before, and the grant
/// itself is strict.
#[test]
fn workspace_grants_default_to_empty_on_every_v1_body_and_are_strict() {
    let consent: OAuthConsentDecisionRequestV1 = serde_json::from_value(json!({
        "version": 1,
        "request": "signed",
        "decision": "approve",
        "name": "Claude",
        "access": "read_write",
        "profiles": [{ "profile_id": PROFILE, "profile_access": "allowed" }]
    }))
    .unwrap();
    assert!(consent.workspaces.is_empty());
    assert!(format!("{consent:?}").contains("workspaces: 0"));

    let update: UpdateAppConnectionRequestV1 =
        serde_json::from_value(json!({ "version": 1, "name": "Renamed" })).unwrap();
    assert!(update.workspaces.is_empty());

    let summary: AppConnectionSummaryV1 = serde_json::from_value(json!({
        "version": 1,
        "connection_id": "50000000-0000-4000-8000-000000000005",
        "name": "Claude",
        "client": { "client_id": "dcr:60000000-0000-4000-8000-000000000006", "name": "Claude", "verified": false, "domain": null },
        "access": "read",
        "allow_profiles_enabled_later": false,
        "profiles": [],
        "created_at_millis": 1,
        "last_used_at_millis": 1,
        "revoked_at_millis": null,
        "revoked_reason": null
    }))
    .unwrap();
    assert!(summary.workspaces.is_empty());

    let grant = AppConnectionWorkspaceGrantV1 {
        workspace_id: workspace(),
        workspace_access: ProfileAccessV1::Allowed,
    };
    assert_eq!(
        round_trip(&grant),
        json!({ "workspace_id": WORKSPACE, "workspace_access": "allowed" })
    );
    assert!(
        serde_json::from_value::<AppConnectionWorkspaceGrantV1>(
            json!({ "workspace_id": WORKSPACE, "workspace_access": "allowed", "profile_id": PROFILE })
        )
        .is_err(),
        "a grant names one scope"
    );
    let decision: koru_account_core::app_connections::WorkspaceDecision = grant.into();
    assert_eq!(decision.workspace_id, workspace());
}

/// The internal state read carries the pending Workspaces and the
/// evaluation key additively: a service without them still parses, and the
/// key round-trips through the core type unchanged.
#[test]
fn the_connection_state_carries_pending_and_the_evaluation_key_additively() {
    let without: AppConnectionStateV1 = serde_json::from_value(json!({
        "version": 1,
        "connection_id": "50000000-0000-4000-8000-000000000005",
        "koru_identity_id": "10000000-0000-4000-8000-000000000001",
        "client_id": "dcr:60000000-0000-4000-8000-000000000006",
        "state": "live",
        "access": "read_write",
        "subscription": "active",
        "active_until_millis": null,
        "allow_profiles_enabled_later": false,
        "profiles": [],
        "workspaces": [],
        "read_at_millis": 1,
        "name": "Claude"
    }))
    .unwrap();
    assert!(without.workspace_grants.is_empty());
    assert!(without.pending.is_empty());
    assert_eq!(without.evaluation, None);

    let pending = GrantPending {
        workspace_id: workspace(),
        retry_after_seconds: 5,
        reason: PendingReason::ProjectionLag {
            watermark: 1,
            admitted: 2,
        },
    };
    let wire: AppConnectionPendingWorkspaceV1 = pending.into();
    assert_eq!(
        round_trip(&wire),
        json!({
            "workspace_id": WORKSPACE,
            "retry_after_seconds": 5,
            "reason": { "kind": "projection_lag", "watermark": 1, "admitted": 2 }
        })
    );
    assert_eq!(GrantPending::from(wire), pending);

    let evaluation = GrantEvaluation {
        revisions: BTreeMap::from([(
            workspace(),
            WorkspaceRevision {
                membership_epoch: 3,
                authorization_version: 4,
                cloud_access_epoch: 5,
                documents_write: true,
                placement: ProjectedPlacement::Projected {
                    profile_id: Some(profile()),
                    placement_revision: 2,
                    lifecycle: SubjectLifecycle::Live,
                    profile_lifecycle: Some(SubjectLifecycle::Live),
                },
            },
        )]),
        follows_placement: true,
    };
    let wire = AppConnectionEvaluationV1::from(&evaluation);
    assert_eq!(
        round_trip(&wire),
        json!({
            "follows_placement": true,
            "workspaces": [{
                "workspace_id": WORKSPACE,
                "membership_epoch": 3,
                "authorization_version": 4,
                "cloud_access_epoch": 5,
                "documents_write": true,
                "placement": {
                    "state": "projected",
                    "profile_id": PROFILE,
                    "placement_revision": 2,
                    "lifecycle": "live",
                    "profile_lifecycle": "live"
                }
            }]
        })
    );
    assert_eq!(GrantEvaluation::from(wire), evaluation);
    let pending_key = AppConnectionPlacementRevisionV1::Pending;
    assert_eq!(round_trip(&pending_key), json!({ "state": "pending" }));
}
