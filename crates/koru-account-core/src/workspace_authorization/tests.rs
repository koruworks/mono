// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

const SCOPES: [WorkspaceScope; 8] = [
    WorkspaceScope::WorkspaceRead,
    WorkspaceScope::WorkspaceWrite,
    WorkspaceScope::DocumentsRead,
    WorkspaceScope::DocumentsWrite,
    WorkspaceScope::TasksRead,
    WorkspaceScope::TasksWrite,
    WorkspaceScope::WorkspaceAdmin,
    WorkspaceScope::WorkspaceSync,
];

// Rows are the contract, independent of required_scope/principal_allowed.
// Columns: action, scope bit, USER_SESSION / DEVICE / CLOUD_REPLICA /
// KORU_SERVICE / APP_CONNECTION.
const MATRIX: [(WorkspaceAction, u8, [bool; 5]); 12] = [
    (
        WorkspaceAction::ReadWorkspace,
        1,
        [true, true, false, false, true],
    ),
    (
        WorkspaceAction::UpdateWorkspace,
        2,
        [true, true, false, false, false],
    ),
    (
        WorkspaceAction::ReadDocument,
        4,
        [true, true, false, false, true],
    ),
    (
        WorkspaceAction::CreateDocument,
        8,
        [true, true, false, false, true],
    ),
    (
        WorkspaceAction::MutateDocument,
        8,
        [true, true, false, false, true],
    ),
    (
        WorkspaceAction::ReadTasks,
        16,
        [true, true, false, false, true],
    ),
    (
        WorkspaceAction::CreateTask,
        32,
        [true, true, false, false, true],
    ),
    (
        WorkspaceAction::UpdateTask,
        32,
        [true, true, false, false, true],
    ),
    (
        WorkspaceAction::EnableCloud,
        64,
        [true, true, false, false, false],
    ),
    (
        WorkspaceAction::DisableCloud,
        64,
        [true, true, false, false, false],
    ),
    (
        WorkspaceAction::DeleteCloud,
        64,
        [true, true, false, false, false],
    ),
    (
        WorkspaceAction::Replicate,
        128,
        [false, true, true, false, true],
    ),
];

fn regional() -> RegionalAuthorization {
    RegionalAuthorization::new(
        Region::Us,
        "https://us.test.invalid".into(),
        "test-regional-api".into(),
        "test-workspace-sync".into(),
    )
    .unwrap()
}

fn principals(owner: KoruIdentityId, workspace_id: WorkspaceId) -> [Principal; 5] {
    [
        Principal::UserSession {
            account_id: owner,
            session_id: AccountSessionId::mint(),
        },
        Principal::Device {
            account_id: owner,
            device_id: DeviceId::new(),
        },
        Principal::CloudReplica {
            replica_id: Uuid::new_v4(),
            workspace_id,
        },
        Principal::KoruService {
            service_id: Uuid::new_v4(),
        },
        Principal::AppConnection {
            account_id: owner,
            connection_id: AppConnectionId::mint(),
        },
    ]
}

fn replica_id(principal: Principal) -> Uuid {
    match principal {
        Principal::CloudReplica { replica_id, .. } => replica_id,
        _ => Uuid::from_u128(42),
    }
}

fn evidence(
    principal: Principal,
    action: WorkspaceAction,
    workspace_id: WorkspaceId,
) -> WorkspaceAuthentication {
    WorkspaceAuthentication {
        principal,
        region: Region::Us,
        issuer: "https://us.test.invalid".into(),
        audience: if action == WorkspaceAction::Replicate {
            "test-workspace-sync"
        } else {
            "test-regional-api"
        }
        .into(),
        scopes: SCOPES.to_vec(),
        binding: (action == WorkspaceAction::Replicate).then_some(
            WorkspaceCredentialBinding::Sync {
                workspace_id,
                membership_epoch: 37,
                replica_id: replica_id(principal),
            },
        ),
    }
}

fn grant(principal: Principal, workspace_id: WorkspaceId, owner: KoruIdentityId) -> WorkspaceGrant {
    WorkspaceGrant {
        principal,
        workspace_id,
        region: Region::Us,
        owner_account_id: owner,
        scopes: SCOPES.to_vec(),
        membership_epoch: 37,
        replica_id: Some(replica_id(principal)),
    }
}

fn scope_set(mask: u8) -> Vec<WorkspaceScope> {
    SCOPES
        .iter()
        .enumerate()
        .filter_map(|(bit, scope)| (mask & (1 << bit) != 0).then_some(*scope))
        .collect()
}

#[test]
fn every_principal_action_and_scope_subset_has_an_explicit_outcome_and_code() {
    let regional = regional();
    let owner = KoruIdentityId::mint();
    let workspace_id = WorkspaceId::new();
    let mut checks = 0;
    for (column, principal) in principals(owner, workspace_id).into_iter().enumerate() {
        for (action, required_bit, allowed) in MATRIX {
            let request = WorkspaceRequest {
                workspace_id,
                action,
            };
            for mask in 0..=u8::MAX {
                // Exercise credential restriction and authority restriction
                // independently; neither can upgrade the other one's scope.
                for restrict_credential in [false, true] {
                    let mut authentication = evidence(principal, action, workspace_id);
                    let mut grant = grant(principal, workspace_id, owner);
                    if restrict_credential {
                        authentication.scopes = scope_set(mask);
                    } else {
                        grant.scopes = scope_set(mask);
                    }
                    let expected = if !allowed[column] {
                        Some("principal_not_allowed")
                    } else if mask & required_bit == 0 {
                        Some("insufficient_scope")
                    } else {
                        None
                    };
                    let decision =
                        decide_workspace_access(&regional, &authentication, Some(&grant), request);
                    assert_eq!(
                        decision.result.err().map(WorkspaceRefusal::code),
                        expected,
                        "{principal:?} {action:?} mask={mask} credential={restrict_credential}"
                    );
                    assert_eq!(decision.principal, Some(principal));
                    assert_eq!(decision.request, request);
                    checks += 1;
                }
            }
        }
    }
    assert_eq!(checks, 5 * 12 * 256 * 2);
}

#[test]
fn collaborator_admin_scope_does_not_grant_cloud_lifecycle_ownership() {
    let owner = KoruIdentityId::mint();
    let collaborator = KoruIdentityId::mint();
    let workspace_id = WorkspaceId::new();
    for principal in principals(collaborator, workspace_id).into_iter().take(2) {
        for (action, _, _) in MATRIX.into_iter().take(11) {
            let result = decide_workspace_access(
                &regional(),
                &evidence(principal, action, workspace_id),
                Some(&grant(principal, workspace_id, owner)),
                WorkspaceRequest {
                    workspace_id,
                    action,
                },
            )
            .result;
            let expected = match action {
                WorkspaceAction::EnableCloud
                | WorkspaceAction::DisableCloud
                | WorkspaceAction::DeleteCloud => Err(WorkspaceRefusal::OwnerRequired),
                _ => Ok(()),
            };
            assert_eq!(result, expected, "{action:?}");
        }
    }
}

#[test]
fn wrong_authority_and_resource_evidence_fail_closed_with_audit_identity() {
    let owner = KoruIdentityId::mint();
    let workspace_id = WorkspaceId::new();
    let other_workspace = WorkspaceId::new();
    let principal = principals(owner, workspace_id)[1];
    let request = WorkspaceRequest {
        workspace_id,
        action: WorkspaceAction::ReadDocument,
    };
    let authentication = evidence(principal, request.action, workspace_id);
    let access = grant(principal, workspace_id, owner);
    for (case, expected) in [
        ("credential_region", WorkspaceRefusal::WrongRegion),
        ("credential_issuer", WorkspaceRefusal::WrongIssuer),
        ("credential_audience", WorkspaceRefusal::WrongAudience),
        ("grant_region", WorkspaceRefusal::WrongRegion),
        ("grant_workspace", WorkspaceRefusal::WorkspaceNotAuthorized),
        ("grant_principal", WorkspaceRefusal::WorkspaceNotAuthorized),
        ("missing_grant", WorkspaceRefusal::WorkspaceNotAuthorized),
    ] {
        let mut authentication = authentication.clone();
        let mut access = access.clone();
        match case {
            "credential_region" => authentication.region = Region::Eu,
            "credential_issuer" => authentication.issuer.push('/'),
            "credential_audience" => authentication.audience = "test-workspace-sync".into(),
            "grant_region" => access.region = Region::Eu,
            "grant_workspace" => access.workspace_id = other_workspace,
            "grant_principal" => access.principal = principals(owner, workspace_id)[0],
            "missing_grant" => (),
            _ => unreachable!(),
        }
        let decision = decide_workspace_access(
            &regional(),
            &authentication,
            (case != "missing_grant").then_some(&access),
            request,
        );
        assert_eq!(decision.result, Err(expected), "{case}");
        assert_eq!(decision.principal, Some(principal));
        assert_eq!(decision.request, request);
    }
}

#[test]
fn cloud_replica_cannot_cross_its_workspace_even_with_an_erroneous_grant() {
    let owner = KoruIdentityId::mint();
    let workspace_id = WorkspaceId::new();
    let principal = principals(owner, WorkspaceId::new())[2];
    let action = WorkspaceAction::Replicate;
    assert_eq!(
        decide_workspace_access(
            &regional(),
            &evidence(principal, action, workspace_id),
            Some(&grant(principal, workspace_id, owner)),
            WorkspaceRequest {
                workspace_id,
                action
            }
        )
        .result,
        Err(WorkspaceRefusal::WorkspaceNotAuthorized)
    );
}

#[test]
fn api_and_sync_audiences_are_not_interchangeable_and_issuer_is_exact() {
    let owner = KoruIdentityId::mint();
    let workspace_id = WorkspaceId::new();
    let principal = principals(owner, workspace_id)[1];
    for (action, wrong_audience) in [
        (WorkspaceAction::Replicate, "test-regional-api"),
        (WorkspaceAction::ReadWorkspace, "test-workspace-sync"),
    ] {
        let mut authentication = evidence(principal, action, workspace_id);
        authentication.audience = wrong_audience.into();
        assert_eq!(
            decide_workspace_access(
                &regional(),
                &authentication,
                Some(&grant(principal, workspace_id, owner)),
                WorkspaceRequest {
                    workspace_id,
                    action
                }
            )
            .result,
            Err(WorkspaceRefusal::WrongAudience)
        );
    }
    assert!(
        RegionalAuthorization::new(Region::Us, "issuer".into(), "same".into(), "same".into())
            .is_err()
    );
    assert!(
        RegionalAuthorization::new(Region::Us, " ".into(), "api".into(), "sync".into()).is_err()
    );
    let mut eu = regional();
    eu.region = Region::Eu;
    let mut authentication = evidence(principal, WorkspaceAction::ReadWorkspace, workspace_id);
    authentication.region = Region::Eu;
    let mut access = grant(principal, workspace_id, owner);
    access.region = Region::Eu;
    assert_eq!(
        decide_workspace_access(
            &eu,
            &authentication,
            Some(&access),
            WorkspaceRequest {
                workspace_id,
                action: WorkspaceAction::ReadWorkspace
            }
        )
        .result,
        Ok(())
    );
}

struct FakeAuthentication(Result<WorkspaceAuthentication, WorkspaceAuthenticationError>);

impl WorkspaceAuthenticator for FakeAuthentication {
    fn authenticate<'a>(
        &'a self,
        _: &'a OpaqueSecret,
    ) -> PortFuture<'a, Result<WorkspaceAuthentication, WorkspaceAuthenticationError>> {
        Box::pin(async { self.0.clone() })
    }
}

struct FakeAuthority {
    result: Mutex<Result<Option<WorkspaceGrant>, WorkspaceAuthorityUnavailable>>,
    reads: AtomicUsize,
}

impl WorkspaceAuthority for FakeAuthority {
    fn read_grant<'a>(
        &'a self,
        principal: Principal,
        workspace_id: WorkspaceId,
    ) -> PortFuture<'a, Result<Option<WorkspaceGrant>, WorkspaceAuthorityUnavailable>> {
        Box::pin(async move {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let result = self.result.lock().unwrap().clone();
            if let Ok(Some(grant)) = &result {
                assert_eq!(grant.principal, principal);
                assert_eq!(grant.workspace_id, workspace_id);
            }
            result
        })
    }
}

#[test]
fn service_authenticates_before_reading_authority_and_never_caches_a_grant() {
    let regional = regional();
    let owner = KoruIdentityId::mint();
    let workspace_id = WorkspaceId::new();
    let principal = principals(owner, workspace_id)[1];
    let request = WorkspaceRequest {
        workspace_id,
        action: WorkspaceAction::ReadWorkspace,
    };
    let authority = FakeAuthority {
        result: Mutex::new(Ok(Some(grant(principal, workspace_id, owner)))),
        reads: AtomicUsize::new(0),
    };
    let credential = OpaqueSecret(vec![7; 32]);
    for (error, expected) in [
        (
            WorkspaceAuthenticationError::Rejected,
            WorkspaceRefusal::AuthenticationRequired,
        ),
        (
            WorkspaceAuthenticationError::Unavailable,
            WorkspaceRefusal::AuthorizationUnavailable,
        ),
    ] {
        let authentication = FakeAuthentication(Err(error));
        let service = WorkspaceService::new(&regional, &authentication, &authority);
        let refusal = pollster::block_on(service.authorize(&credential, request)).unwrap_err();
        assert_eq!(refusal.result, Err(expected));
        assert_eq!(refusal.principal, None);
        assert_eq!(refusal.request, request);
        assert_eq!(authority.reads.load(Ordering::Relaxed), 0);
    }
    let authentication = FakeAuthentication(Ok(evidence(principal, request.action, workspace_id)));
    let service = WorkspaceService::new(&regional, &authentication, &authority);
    let permit = pollster::block_on(service.authorize(&credential, request)).unwrap();
    assert_eq!(permit.decision().principal, Some(principal));
    assert_eq!(permit.decision().request, request);
    assert_eq!(permit.membership_epoch(), 37);
    assert_eq!(permit.replica_id(), Some(replica_id(principal)));
    *authority.result.lock().unwrap() = Ok(None);
    let refusal = pollster::block_on(service.authorize(&credential, request)).unwrap_err();
    assert_eq!(
        refusal.result,
        Err(WorkspaceRefusal::WorkspaceNotAuthorized)
    );
    *authority.result.lock().unwrap() = Err(WorkspaceAuthorityUnavailable);
    let refusal = pollster::block_on(service.authorize(&credential, request)).unwrap_err();
    assert_eq!(
        refusal.result,
        Err(WorkspaceRefusal::AuthorizationUnavailable)
    );
    assert_eq!(refusal.principal, Some(principal));
    assert_eq!(authority.reads.load(Ordering::Relaxed), 3);
}

#[test]
fn stable_refusal_codes_have_one_serialized_spelling() {
    for (refusal, code) in [
        (
            WorkspaceRefusal::AuthenticationRequired,
            "authentication_required",
        ),
        (
            WorkspaceRefusal::AuthorizationUnavailable,
            "authorization_unavailable",
        ),
        (WorkspaceRefusal::WrongRegion, "wrong_region"),
        (WorkspaceRefusal::WrongIssuer, "wrong_issuer"),
        (WorkspaceRefusal::WrongAudience, "wrong_audience"),
        (
            WorkspaceRefusal::PrincipalNotAllowed,
            "principal_not_allowed",
        ),
        (
            WorkspaceRefusal::WorkspaceNotAuthorized,
            "workspace_not_authorized",
        ),
        (WorkspaceRefusal::OwnerRequired, "owner_required"),
        (WorkspaceRefusal::InsufficientScope, "insufficient_scope"),
        (
            WorkspaceRefusal::SyncBindingRequired,
            "sync_binding_required",
        ),
        (
            WorkspaceRefusal::MembershipEpochMismatch,
            "membership_epoch_mismatch",
        ),
        (WorkspaceRefusal::ReplicaMismatch, "replica_mismatch"),
    ] {
        assert_eq!(refusal.code(), code);
        assert!(
            code.bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        );
        let wire = serde_json::to_string(&refusal).unwrap();
        assert_eq!(serde_json::from_str::<String>(&wire).unwrap(), code);
        assert_eq!(
            serde_json::from_str::<WorkspaceRefusal>(&wire).unwrap(),
            refusal
        );
    }
    assert_eq!(serde_json::to_string(&Region::Us).unwrap(), "\"US\"");
    assert!(serde_json::from_str::<WorkspaceRefusal>("\"WRONG_REGION\"").is_err());
    assert_eq!(
        SCOPES
            .iter()
            .map(|scope| serde_json::to_value(scope).unwrap())
            .collect::<Vec<_>>(),
        [
            "workspace:read",
            "workspace:write",
            "documents:read",
            "documents:write",
            "tasks:read",
            "tasks:write",
            "workspace:admin",
            "workspace:sync"
        ]
    );
}

#[test]
fn signed_sync_workspace_epoch_and_replica_bindings_are_never_upgraded_by_a_grant() {
    let owner = KoruIdentityId::mint();
    let workspace_id = WorkspaceId::new();
    let other_workspace = WorkspaceId::new();
    let action = WorkspaceAction::Replicate;
    let request = WorkspaceRequest {
        workspace_id,
        action,
    };
    let all = principals(owner, workspace_id);
    for principal in [all[1], all[2], all[4]] {
        let authentication = evidence(principal, action, workspace_id);
        let access = grant(principal, workspace_id, owner);
        for (binding, expected) in [
            (None, WorkspaceRefusal::SyncBindingRequired),
            (
                Some(WorkspaceCredentialBinding::Workspace {
                    workspace_id,
                    membership_epoch: 37,
                }),
                WorkspaceRefusal::SyncBindingRequired,
            ),
            (
                Some(WorkspaceCredentialBinding::Sync {
                    workspace_id: other_workspace,
                    membership_epoch: 37,
                    replica_id: replica_id(principal),
                }),
                WorkspaceRefusal::WorkspaceNotAuthorized,
            ),
            (
                Some(WorkspaceCredentialBinding::Sync {
                    workspace_id,
                    membership_epoch: 36,
                    replica_id: replica_id(principal),
                }),
                WorkspaceRefusal::MembershipEpochMismatch,
            ),
            (
                Some(WorkspaceCredentialBinding::Sync {
                    workspace_id,
                    membership_epoch: 38,
                    replica_id: replica_id(principal),
                }),
                WorkspaceRefusal::MembershipEpochMismatch,
            ),
            (
                Some(WorkspaceCredentialBinding::Sync {
                    workspace_id,
                    membership_epoch: 37,
                    replica_id: Uuid::new_v4(),
                }),
                WorkspaceRefusal::ReplicaMismatch,
            ),
        ] {
            let mut authentication = authentication.clone();
            authentication.binding = binding;
            assert_eq!(
                decide_workspace_access(&regional(), &authentication, Some(&access), request)
                    .result,
                Err(expected),
                "{principal:?} {binding:?}"
            );
        }
        let mut missing_replica = access.clone();
        missing_replica.replica_id = None;
        assert_eq!(
            decide_workspace_access(
                &regional(),
                &authentication,
                Some(&missing_replica),
                request
            )
            .result,
            Err(WorkspaceRefusal::ReplicaMismatch)
        );
        if let Principal::CloudReplica { .. } = principal {
            let another_replica = Uuid::new_v4();
            let mut erroneous_grant = access.clone();
            erroneous_grant.replica_id = Some(another_replica);
            let mut authentication = authentication.clone();
            authentication.binding = Some(WorkspaceCredentialBinding::Sync {
                workspace_id,
                membership_epoch: 37,
                replica_id: another_replica,
            });
            assert_eq!(
                decide_workspace_access(
                    &regional(),
                    &authentication,
                    Some(&erroneous_grant),
                    request
                )
                .result,
                Err(WorkspaceRefusal::ReplicaMismatch)
            );
        }
    }
    // The same device has legitimate grants for both workspaces. Its token for
    // A must not authorize B merely because the authority returns B's grant.
    let principal = principals(owner, workspace_id)[1];
    let authentication = evidence(principal, action, workspace_id);
    assert_eq!(
        decide_workspace_access(
            &regional(),
            &authentication,
            Some(&grant(principal, other_workspace, owner)),
            WorkspaceRequest {
                workspace_id: other_workspace,
                action
            }
        )
        .result,
        Err(WorkspaceRefusal::WorkspaceNotAuthorized)
    );
}

#[test]
fn restricted_api_credentials_keep_their_workspace_and_epoch_too() {
    let owner = KoruIdentityId::mint();
    let workspace_id = WorkspaceId::new();
    let principal = principals(owner, workspace_id)[0];
    let action = WorkspaceAction::ReadDocument;
    let request = WorkspaceRequest {
        workspace_id,
        action,
    };
    let access = grant(principal, workspace_id, owner);
    for (bound_workspace, epoch, expected) in [
        (workspace_id, 37, Ok(())),
        (
            WorkspaceId::new(),
            37,
            Err(WorkspaceRefusal::WorkspaceNotAuthorized),
        ),
        (
            workspace_id,
            36,
            Err(WorkspaceRefusal::MembershipEpochMismatch),
        ),
    ] {
        let mut authentication = evidence(principal, action, workspace_id);
        authentication.binding = Some(WorkspaceCredentialBinding::Workspace {
            workspace_id: bound_workspace,
            membership_epoch: epoch,
        });
        assert_eq!(
            decide_workspace_access(&regional(), &authentication, Some(&access), request).result,
            expected
        );
    }
}

#[test]
fn app_connection_access_decides_writes_and_tasks_follow_documents() {
    let owner = KoruIdentityId::mint();
    let workspace_id = WorkspaceId::new();
    let principal = principals(owner, workspace_id)[4];
    for access in [AppConnectionAccess::Read, AppConnectionAccess::ReadWrite] {
        // The connection's evidence and its live grant both carry the scopes
        // of its access, plus replication for the sync credential's path.
        let mut scopes = access.scopes().to_vec();
        scopes.push(WorkspaceScope::WorkspaceSync);
        for (action, _, allowed) in MATRIX {
            let mut authentication = evidence(principal, action, workspace_id);
            authentication.scopes = scopes.clone();
            let mut access_grant = grant(principal, workspace_id, owner);
            access_grant.scopes = scopes.clone();
            let expected = if !allowed[4] {
                Err(WorkspaceRefusal::PrincipalNotAllowed)
            } else if access == AppConnectionAccess::Read
                && matches!(
                    action,
                    WorkspaceAction::CreateDocument
                        | WorkspaceAction::MutateDocument
                        | WorkspaceAction::CreateTask
                        | WorkspaceAction::UpdateTask
                )
            {
                Err(WorkspaceRefusal::InsufficientScope)
            } else {
                Ok(())
            };
            assert_eq!(
                decide_workspace_access(
                    &regional(),
                    &authentication,
                    Some(&access_grant),
                    WorkspaceRequest {
                        workspace_id,
                        action
                    }
                )
                .result,
                expected,
                "{access:?} {action:?}"
            );
        }
    }
    assert_eq!(
        serde_json::to_value([AppConnectionAccess::Read, AppConnectionAccess::ReadWrite]).unwrap(),
        serde_json::json!(["read", "read_write"])
    );
    assert!(serde_json::from_str::<AppConnectionAccess>("\"write\"").is_err());
}

#[test]
fn app_connection_never_controls_the_cloud_lifecycle_even_as_the_owner() {
    let owner = KoruIdentityId::mint();
    let workspace_id = WorkspaceId::new();
    let principal = principals(owner, workspace_id)[4];
    assert_eq!(principal.account_id(), Some(owner));
    for action in [
        WorkspaceAction::UpdateWorkspace,
        WorkspaceAction::EnableCloud,
        WorkspaceAction::DisableCloud,
        WorkspaceAction::DeleteCloud,
    ] {
        assert_eq!(
            decide_workspace_access(
                &regional(),
                &evidence(principal, action, workspace_id),
                Some(&grant(principal, workspace_id, owner)),
                WorkspaceRequest {
                    workspace_id,
                    action
                }
            )
            .result,
            Err(WorkspaceRefusal::PrincipalNotAllowed),
            "{action:?}"
        );
    }
    // A grant read for another connection of the same account is not this
    // connection's authority.
    let other = Principal::AppConnection {
        account_id: owner,
        connection_id: AppConnectionId::mint(),
    };
    assert_eq!(
        decide_workspace_access(
            &regional(),
            &evidence(principal, WorkspaceAction::ReadDocument, workspace_id),
            Some(&grant(other, workspace_id, owner)),
            WorkspaceRequest {
                workspace_id,
                action: WorkspaceAction::ReadDocument
            }
        )
        .result,
        Err(WorkspaceRefusal::WorkspaceNotAuthorized)
    );
}
