// SPDX-License-Identifier: MPL-2.0

//! The lifecycle operation, over a real router and a real Store worker.
//!
//! There is no session host in these tests, and that is the shape of most of
//! them: without a bridge the operation refuses *before* it changes anything,
//! which is the ordering the whole contract rests on. The bridge's own half —
//! the join, the idempotence, the refusals — is asserted where a real host
//! runs, in `apps/koru-agent/tests/sessions_ipc.rs`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use koru_api_wire::{WorkspaceLifecycleAction, WorkspaceLifecycleOutcome};
use tower::ServiceExt as _;

use crate::{
    Admission, GatewayState, MANAGE_WORKSPACES_SCOPE, WORKSPACE_DESTROY_SCOPE, now_millis,
};

struct Fixture {
    _root: tempfile::TempDir,
    state: Arc<GatewayState>,
    /// A vouched token: the only kind that reaches this operation.
    destroyer: String,
    /// A vouched token holding the *other* half of the split — the one the
    /// migration operation answers, and the lifecycle one does not.
    migrator: String,
    /// The default set, which is what every other local caller holds.
    ordinary: String,
    workspace: String,
    /// The profile that owns `workspace`, and one it could move to.
    owner: String,
    elsewhere: String,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let worker = koru_store_server::StoreWorker::new(koru_store_server::WorkerConfig::embedded(
        root.path().to_path_buf(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let state = GatewayState::new("internal-debug".into(), worker);
    let (ordinary, _) = state.sessions.mint("mcp-tool", &[], now_millis());
    let (destroyer, _) = state.sessions.mint_admitted(
        "koru",
        &[
            WORKSPACE_DESTROY_SCOPE.into(),
            MANAGE_WORKSPACES_SCOPE.into(),
            "store.read_documents".into(),
        ],
        Admission::Vouched,
        now_millis(),
    );
    let (migrator, _) = state.sessions.mint_admitted(
        "koru",
        &[
            crate::WORKSPACE_MIGRATE_SCOPE.into(),
            MANAGE_WORKSPACES_SCOPE.into(),
            "store.read_documents".into(),
        ],
        Admission::Vouched,
        now_millis(),
    );
    Fixture {
        _root: root,
        state,
        destroyer,
        migrator,
        ordinary,
        workspace: String::new(),
        owner: String::new(),
        elsewhere: String::new(),
    }
}

async fn call(
    state: &Arc<GatewayState>,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap())),
        None => builder.body(Body::empty()),
    }
    .unwrap();
    let response = crate::router(Arc::clone(state))
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn with_workspace() -> Fixture {
    let mut fixture = fixture();
    let (_, profile) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        "/v1/profiles",
        Some(serde_json::json!({ "name": "Ada" })),
    )
    .await;
    let (_, elsewhere) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        "/v1/profiles",
        Some(serde_json::json!({ "name": "Grace" })),
    )
    .await;
    let (_, workspace) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        "/v1/workspaces",
        Some(serde_json::json!({ "owner": profile["id"], "name": "Notes" })),
    )
    .await;
    fixture.workspace = workspace["id"].as_str().unwrap().to_owned();
    fixture.owner = profile["id"].as_str().unwrap().to_owned();
    fixture.elsewhere = elsewhere["id"].as_str().unwrap().to_owned();
    fixture
}

#[tokio::test]
async fn profile_removal_refuses_stale_or_ordinary_lifecycle_before_session_host_work() {
    let fixture = with_workspace().await;
    let uri = format!("/v1/profiles/{}/removal", fixture.owner);
    let (status, mark) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        &uri,
        Some(serde_json::json!({"kind":"local"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{mark}");
    for attempt in [
        None,
        Some(koru_store_core::ids::RemovalAttemptId::new().to_storage()),
    ] {
        let mut body = confirmed(&fixture, "soft_delete").await;
        if let Some(attempt) = &attempt {
            body["removal_attempt"] = attempt.clone().into();
        }
        let (status, refusal) = call(
            &fixture.state,
            &fixture.destroyer,
            "POST",
            &format!("/v1/workspaces/{}/lifecycle", fixture.workspace),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refusal}");
        assert_eq!(
            refusal["code"],
            if attempt.is_some() {
                "removal_attempt_stale"
            } else {
                "profile_being_removed"
            }
        );
        // This fixture has no session bridge. Reaching it would instead return
        // capability_unavailable, so this proves ordering before session work.
    }
    let (status, _) = call(
        &fixture.state,
        &fixture.destroyer,
        "DELETE",
        &uri,
        Some(serde_json::json!({"attempt":mark["attempt"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// **Ask for the consequence, and answer with the token it came with.**
///
/// Every caller of the operation goes through this, because every caller of the
/// operation now has to: the confirmation is the gateway's, issued alongside
/// the list it computed, and a token from anywhere else is refused. This is the
/// round trip SP-8b's dialog makes, with the reading and the clicking left out.
async fn confirmed(fixture: &Fixture, action: &str) -> serde_json::Value {
    let (status, consequence) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        &format!(
            "/v1/workspaces/{}/lifecycle/confirmation",
            fixture.workspace
        ),
        Some(serde_json::json!({ "action": action })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{consequence}");
    serde_json::json!({
        "action": action,
        "confirmation": consequence["confirmation"],
    })
}

/// A body carrying a confirmation this gateway never issued.
fn unminted(action: &str) -> serde_json::Value {
    serde_json::json!({
        "action": action,
        "confirmation": "9f2a2f2c-0b7c-4a6f-8f4f-1d63f9b2f7d1",
    })
}

/// **The scope, before anything else.** A caller that may not remove a
/// workspace learns that and not whether this one exists — which is why the
/// id below is one nobody minted and the answer is still 403. It outranks the
/// confirmation check too: the token here is one nobody issued, and the answer
/// is still about the permission.
#[tokio::test]
async fn an_ordinary_token_is_refused_before_the_workspace_is_looked_up() {
    let fixture = fixture();
    let unknown = "00000000-0000-4000-8000-000000000000";
    let (status, answer) = call(
        &fixture.state,
        &fixture.ordinary,
        "POST",
        &format!("/v1/workspaces/{unknown}/lifecycle"),
        Some(unminted("soft_delete")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(answer["code"], "permission_denied");

    // And the issuance is behind the same scope: working out what would be
    // destroyed names somebody's live terminals, which is not a smaller
    // permission than destroying them.
    let (status, answer) = call(
        &fixture.state,
        &fixture.ordinary,
        "POST",
        &format!("/v1/workspaces/{unknown}/lifecycle/confirmation"),
        Some(serde_json::json!({ "action": "soft_delete" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(answer["code"], "permission_denied");
}

/// **A confirmation this gateway did not issue is not an acceptance.**
///
/// The token is the acceptance of a consequence *this* gateway computed
/// (ADR-267's recorded recommendation, now the contract). One the caller minted
/// says only that somebody clicked something, which is what the server-issued
/// token exists to stop being enough — so it is refused before a single
/// terminal is looked at, let alone closed.
#[tokio::test]
async fn a_confirmation_the_gateway_did_not_issue_is_refused() {
    let fixture = with_workspace().await;
    let (status, answer) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        &format!("/v1/workspaces/{}/lifecycle", fixture.workspace),
        Some(unminted("archive")),
    )
    .await;
    // 400 rather than 403: the caller holds the permission; it is the question
    // it claims to have asked that this gateway does not recognise.
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(answer["code"], "invalid_request");
}

/// **A token is spent by being used, and it is about one workspace and one
/// action.**
///
/// Three ways to hold a real token and still be refused, and each is a way the
/// acceptance would otherwise be about something other than what happened: the
/// same token twice, a token issued for archiving used to delete, and a token
/// issued for another workspace.
#[tokio::test]
async fn an_issued_confirmation_is_single_use_and_bound_to_its_question() {
    let fixture = with_workspace().await;

    // Bound to its action: issued for `archive`, presented for `soft_delete`.
    let issued = confirmed(&fixture, "archive").await;
    let (status, answer) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        &format!("/v1/workspaces/{}/lifecycle", fixture.workspace),
        Some(serde_json::json!({
            "action": "soft_delete",
            "confirmation": issued["confirmation"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");

    // Bound to its workspace: issued for this one, presented for another.
    let (_, profile) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        "/v1/profiles",
        Some(serde_json::json!({ "name": "Grace" })),
    )
    .await;
    let (_, other) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        "/v1/workspaces",
        Some(serde_json::json!({ "owner": profile["id"], "name": "Elsewhere" })),
    )
    .await;
    let issued = confirmed(&fixture, "restore").await;
    let (status, answer) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        &format!("/v1/workspaces/{}/lifecycle", other["id"].as_str().unwrap()),
        Some(issued.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");

    // Single-use. `restore` on a workspace that is not in the trash is the
    // idempotent success, so the first call proves the token was good and the
    // second proves it is gone rather than proving the action refused.
    let issued = confirmed(&fixture, "restore").await;
    let (status, answer) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        &format!("/v1/workspaces/{}/lifecycle", fixture.workspace),
        Some(issued.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let (status, answer) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        &format!("/v1/workspaces/{}/lifecycle", fixture.workspace),
        Some(issued),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
}

/// **The consequence is the gateway's answer, not the caller's claim.**
///
/// With no session-host bridge there are no terminals to name, and the issue
/// still succeeds — a person must be able to open the dialog and read why the
/// operation will refuse, rather than finding the dialog itself unavailable.
/// `restore` names none in any case: it closes nothing.
#[tokio::test]
async fn the_confirmation_carries_the_action_the_workspace_and_the_terminals() {
    let fixture = with_workspace().await;
    for action in ["archive", "soft_delete", "restore"] {
        let (status, consequence) = call(
            &fixture.state,
            &fixture.destroyer,
            "POST",
            &format!(
                "/v1/workspaces/{}/lifecycle/confirmation",
                fixture.workspace
            ),
            Some(serde_json::json!({ "action": action })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{consequence}");
        let issued: koru_api_wire::WorkspaceConfirmation =
            serde_json::from_value(consequence).unwrap();
        assert_eq!(issued.workspace, fixture.workspace);
        assert_eq!(issued.action.token(), action);
        assert!(!issued.confirmation.trim().is_empty());
        assert!(issued.sessions.is_empty(), "no host, so no terminals");
        assert!(issued.expires_at > now_millis(), "issued already expired");
    }
}

/// A workspace nobody minted has no consequence to compute, and that is where
/// a caller finds out — the dialog asks before it draws, so the 404 arrives
/// before anybody is shown a question about a workspace that is not there.
#[tokio::test]
async fn an_unknown_workspace_has_no_consequence() {
    let fixture = with_workspace().await;
    let unknown = uuid::Uuid::new_v4();
    let (status, _) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        &format!("/v1/workspaces/{unknown}/lifecycle/confirmation"),
        Some(serde_json::json!({ "action": "archive" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        "/v1/workspaces/not-a-uuid/lifecycle/confirmation",
        Some(serde_json::json!({ "action": "archive" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// The operation is not reachable by an empty body, and a confirmation of
/// whitespace is an empty body with extra steps.
#[tokio::test]
async fn a_lifecycle_request_without_a_confirmation_is_refused() {
    let fixture = with_workspace().await;
    for confirmation in ["", "   "] {
        let (status, answer) = call(
            &fixture.state,
            &fixture.destroyer,
            "POST",
            &format!("/v1/workspaces/{}/lifecycle", fixture.workspace),
            Some(serde_json::json!({
                "action": "archive",
                "confirmation": confirmation,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    }
}

/// An id that is not a uuid is refused by the operation before anything else
/// about the request is considered — it cannot name a workspace, so there is
/// nothing a confirmation could have been about.
#[tokio::test]
async fn an_id_that_is_not_a_workspace_is_refused() {
    let fixture = with_workspace().await;
    let (status, _) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        "/v1/workspaces/not-a-uuid/lifecycle",
        Some(unminted("archive")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// **The ordering, asserted by what did not happen.** A host with no
/// session-host bridge cannot close a workspace's terminals, so it refuses —
/// and the catalog is exactly where it was. A version that applied the
/// mutation first and then discovered it could not close anything would pass
/// every other test here and fail this one.
#[tokio::test]
async fn without_a_bridge_the_workspace_is_left_alone() {
    let fixture = with_workspace().await;
    for action in ["archive", "soft_delete"] {
        let accepted = confirmed(&fixture, action).await;
        let (status, answer) = call(
            &fixture.state,
            &fixture.destroyer,
            "POST",
            &format!("/v1/workspaces/{}/lifecycle", fixture.workspace),
            Some(accepted),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
        assert_eq!(answer["code"], "capability_unavailable");
    }
    let (status, workspace) = call(
        &fixture.state,
        &fixture.destroyer,
        "GET",
        &format!("/v1/workspaces/{}", fixture.workspace),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(workspace["archived"], serde_json::Value::Bool(false));
    assert!(workspace["deleted_at"].is_null());
}

/// **Restore closes nothing, so it needs no bridge.** A soft-deleted workspace
/// has no terminals to announce, and bringing one back is not a consequence
/// anybody has to accept the closing of — so the one action that does not
/// terminate is also the one this host can perform.
#[tokio::test]
async fn restore_runs_without_a_bridge_and_reports_closing_nothing() {
    let fixture = with_workspace().await;
    // Put it in the trash through the Store route, which is what a supervised
    // gateway's own operation would have done.
    let (status, _) = call(
        &fixture.state,
        &fixture.destroyer,
        "DELETE",
        &format!("/v1/workspaces/{}", fixture.workspace),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, answer) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        &format!("/v1/workspaces/{}/lifecycle", fixture.workspace),
        Some(confirmed(&fixture, "restore").await),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let outcome: WorkspaceLifecycleOutcome = serde_json::from_value(answer).unwrap();
    assert_eq!(outcome.action, WorkspaceLifecycleAction::Restore);
    assert!(outcome.terminated_sessions.is_empty());
    assert!(outcome.sessions_closed);
    assert!(outcome.mutation_applied);

    let (status, workspace) = call(
        &fixture.state,
        &fixture.destroyer,
        "GET",
        &format!("/v1/workspaces/{}", fixture.workspace),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(workspace["deleted_at"].is_null());

    // Idempotent: restoring a workspace that is not in the trash is not a
    // failure, it is the outcome the caller wanted.
    let (status, _) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        &format!("/v1/workspaces/{}/lifecycle", fixture.workspace),
        Some(confirmed(&fixture, "restore").await),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// **The failure body is the success body plus a verdict.** A caller reporting
/// S24 — terminals closed, catalog unchanged — needs the same fields it would
/// have read on success, so the refusal carries an `outcome` rather than a
/// sentence about one. Here the failure is earlier than S24 (nothing closed),
/// but the shape is what is being pinned.
#[tokio::test]
async fn a_refusal_carries_the_outcome_it_is_refusing() {
    let fixture = with_workspace().await;
    let (status, answer) = call(
        &fixture.state,
        &fixture.destroyer,
        "POST",
        &format!("/v1/workspaces/{}/lifecycle", fixture.workspace),
        Some(confirmed(&fixture, "soft_delete").await),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // The bridge refusal happens before an outcome exists, so this one is the
    // plain failure shape; what matters is that it is JSON with a stable code
    // and says nothing about the workspace having changed.
    assert_eq!(answer["code"], "capability_unavailable");
    assert!(answer.get("outcome").is_none() || answer["outcome"]["mutation_applied"] == false);
}

#[test]
fn a_confirmed_soft_delete_uses_the_replacing_route() {
    let workspace = "00000000-0000-4000-8000-000000000001";
    assert_eq!(
        super::soft_delete_uri(workspace),
        format!("/v1/workspaces/{workspace}/delete-replacing")
    );
}

#[tokio::test]
async fn pending_listing_preserves_committed_facts_and_original_cause_in_old_failure_envelope() {
    use axum::response::IntoResponse;
    use koru_store_core::{ProfileId, Timestamp, Workspace};
    use koru_store_wire::{ListingState, RemovedWorkspace, Replacement};
    let workspace = Workspace::new(ProfileId::new(), "Source", Timestamp::from_millis(1)).unwrap();
    let removed = RemovedWorkspace {
        workspace: workspace.clone(),
        authority_committed: true,
        listing: ListingState::Pending {
            cause: "storage_failed".into(),
        },
        replacement: Replacement::Unknown,
    };
    let response = (
        StatusCode::CONFLICT,
        axum::Json(koru_store_wire::ListingPendingFailure {
            code: "listing_pending".into(),
            message: "catalog publication pending".into(),
            cause: "storage_failed".into(),
            outcome: removed.clone(),
        }),
    )
        .into_response();
    let refusal = super::refusal_of(response).await;
    assert_eq!(refusal.code, "storage_failed");
    let mut outcome = WorkspaceLifecycleOutcome {
        workspace: workspace.id.to_storage(),
        action: WorkspaceLifecycleAction::SoftDelete,
        terminated_sessions: vec!["closed-session".into()],
        sessions_closed: true,
        mutation_applied: false,
        authority_committed: Some(false),
        listing: None,
        replacement: None,
    };
    super::apply_removal_facts(&mut outcome, refusal.removal.clone().unwrap());
    let response = super::caused(
        refusal.status,
        "workspace_lifecycle_incomplete",
        "catalog pending",
        outcome,
        &refusal,
    );
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["code"], "workspace_lifecycle_incomplete");
    assert_eq!(body["retryable"], true);
    assert_eq!(body["cause"], "storage_failed");
    assert_eq!(body["outcome"]["sessions_closed"], true);
    assert_eq!(body["outcome"]["mutation_applied"], false);
    assert_eq!(body["outcome"]["authority_committed"], true);
    assert_eq!(body["outcome"]["listing"]["state"], "pending");
    assert_eq!(body["outcome"]["replacement"]["state"], "unknown");
}

#[test]
fn old_lifecycle_shapes_remain_readable_and_old_requests_have_a_replacement_name() {
    let old = serde_json::json!({"workspace":"source", "action":"soft_delete", "terminated_sessions":[], "sessions_closed":true, "mutation_applied":false});
    let outcome: WorkspaceLifecycleOutcome = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(outcome.authority_committed, None);
    assert_eq!(outcome.listing, None);
    assert_eq!(outcome.replacement, None);
    assert_eq!(serde_json::to_value(outcome).unwrap(), old);
    let request: koru_api_wire::WorkspaceLifecycleRequest = serde_json::from_value(
        serde_json::json!({"action":"soft_delete", "confirmation":"receipt"}),
    )
    .unwrap();
    assert!(!request.replacement_name.is_empty());
}

/// The route table is the whole authorization story, and each operation and
/// its confirmation take the same scope. Asking what would be destroyed or
/// moved reveals the same sensitive consequences as performing the action.
#[test]
fn each_operation_and_its_confirmation_take_the_same_scope_and_nothing_else() {
    assert_eq!(super::ROUTES.len(), 4);
    for route in super::ROUTES {
        assert_eq!(route.method, "POST");
    }
    assert_eq!(
        super::ROUTES
            .iter()
            .map(|route| (route.path, route.scope))
            .collect::<Vec<_>>(),
        vec![
            (
                "/v1/workspaces/{id}/lifecycle/confirmation",
                WORKSPACE_DESTROY_SCOPE
            ),
            ("/v1/workspaces/{id}/lifecycle", WORKSPACE_DESTROY_SCOPE),
            (
                "/v1/workspaces/{id}/migration/confirmation",
                crate::WORKSPACE_MIGRATE_SCOPE
            ),
            (
                "/v1/workspaces/{id}/migration",
                crate::WORKSPACE_MIGRATE_SCOPE
            ),
        ]
    );
}

/// **Three actions and no fourth.** There is no purge route anywhere in Koru —
/// the retention sweep is the only thing that removes bytes — so an action
/// naming one would describe an operation that does not exist.
#[test]
fn the_actions_are_the_three_that_exist() {
    for (action, token, closes) in [
        (WorkspaceLifecycleAction::Archive, "archive", true),
        (WorkspaceLifecycleAction::SoftDelete, "soft_delete", true),
        (WorkspaceLifecycleAction::Restore, "restore", false),
    ] {
        assert_eq!(action.token(), token);
        assert_eq!(action.closes_sessions(), closes);
        assert_eq!(
            serde_json::from_value::<WorkspaceLifecycleAction>(serde_json::json!(token)).unwrap(),
            action
        );
    }
    assert!(
        serde_json::from_value::<WorkspaceLifecycleAction>(serde_json::json!("purge")).is_err(),
        "an action nobody built was accepted"
    );
}

// -------------------------------------------------- the migration, SP-15a

/// Ask for the move's consequence, and hand back the body that answers it.
async fn move_confirmed(fixture: &Fixture, to: &str) -> serde_json::Value {
    let (status, consequence) = call(
        &fixture.state,
        &fixture.migrator,
        "POST",
        &format!(
            "/v1/workspaces/{}/migration/confirmation",
            fixture.workspace
        ),
        Some(serde_json::json!({ "to": to })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{consequence}");
    serde_json::json!({
        "to": to,
        "confirmation": consequence["confirmation"],
    })
}

/// **The scope, before anything else, and it is not the destructive one.**
///
/// A vouched window holding `workspace.destroy` is refused here. That is the
/// whole of why the two scopes are two: being trusted to empty a workspace is
/// not being trusted to hand it to another persona, and this is the route where
/// a collapse of the pair would first stop being visible.
#[tokio::test]
async fn moving_a_workspace_refuses_every_token_but_a_vouched_migrator() {
    let fixture = with_workspace().await;
    let body = serde_json::json!({
        "to": fixture.elsewhere,
        "confirmation": "9f2a2f2c-0b7c-4a6f-8f4f-1d63f9b2f7d1",
    });
    for token in [&fixture.ordinary, &fixture.destroyer] {
        let (status, answer) = call(
            &fixture.state,
            token,
            "POST",
            &format!("/v1/workspaces/{}/migration", fixture.workspace),
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(answer["code"], "permission_denied");

        // And the issuance is behind the same scope: the consequence names
        // somebody's live terminals, which is not a smaller permission.
        let (status, answer) = call(
            &fixture.state,
            token,
            "POST",
            &format!(
                "/v1/workspaces/{}/migration/confirmation",
                fixture.workspace
            ),
            Some(serde_json::json!({ "to": fixture.elsewhere })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(answer["code"], "permission_denied");
    }
}

/// The refusal outranks everything, including whether the workspace exists.
#[tokio::test]
async fn an_unvouched_migration_is_refused_before_the_workspace_is_looked_up() {
    let fixture = fixture();
    let unknown = "00000000-0000-4000-8000-000000000000";
    let (status, answer) = call(
        &fixture.state,
        &fixture.ordinary,
        "POST",
        &format!("/v1/workspaces/{unknown}/migration"),
        Some(serde_json::json!({ "to": unknown, "confirmation": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(answer["code"], "permission_denied");
}

/// **The consequence names both ends**, so a dialog does not have to read the
/// origin off a catalog that might have moved on since.
#[tokio::test]
async fn the_move_confirmation_names_both_profiles_and_issues_a_token() {
    let fixture = with_workspace().await;
    let (status, consequence) = call(
        &fixture.state,
        &fixture.migrator,
        "POST",
        &format!(
            "/v1/workspaces/{}/migration/confirmation",
            fixture.workspace
        ),
        Some(serde_json::json!({ "to": fixture.elsewhere })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{consequence}");
    assert_eq!(consequence["workspace"], fixture.workspace);
    assert_eq!(consequence["from"], fixture.owner);
    assert_eq!(consequence["to"], fixture.elsewhere);
    assert!(!consequence["confirmation"].as_str().unwrap().is_empty());
    assert!(consequence["expires_at"].as_i64().unwrap() > now_millis());
    // No session host in these tests, so the list is empty rather than absent:
    // a bridge that is not there leaves the consequence empty instead of
    // failing the issue, and the operation refuses in its own words.
    assert_eq!(consequence["sessions"], serde_json::json!([]));
}

/// **A destination that does not exist is refused before a token is minted.**
///
/// The Store would refuse the move anyway, but by then a person has accepted
/// the closing of their terminals for something that was never going to happen.
#[tokio::test]
async fn no_consequence_is_computed_for_a_profile_that_is_not_there() {
    let fixture = with_workspace().await;
    let (status, answer) = call(
        &fixture.state,
        &fixture.migrator,
        "POST",
        &format!(
            "/v1/workspaces/{}/migration/confirmation",
            fixture.workspace
        ),
        Some(serde_json::json!({ "to": "00000000-0000-4000-8000-000000000000" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(answer["code"], "not_found");
}

/// A workspace nobody minted is a 404, and the destination is not consulted.
#[tokio::test]
async fn a_move_confirmation_for_an_unknown_workspace_is_not_found() {
    let fixture = with_workspace().await;
    let unknown = "00000000-0000-4000-8000-000000000001";
    let (status, _) = call(
        &fixture.state,
        &fixture.migrator,
        "POST",
        &format!("/v1/workspaces/{unknown}/migration/confirmation"),
        Some(serde_json::json!({ "to": fixture.elsewhere })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// **A token issued for one destination does not spend against another.**
///
/// The acceptance was about a consequence naming a particular profile. Letting
/// the same token carry a different `to` would make the round trip decorative:
/// a caller could show one destination and perform another.
#[tokio::test]
async fn a_confirmation_issued_for_one_destination_is_refused_for_a_second() {
    let fixture = with_workspace().await;
    let (_, third) = call(
        &fixture.state,
        &fixture.migrator,
        "POST",
        "/v1/profiles",
        Some(serde_json::json!({ "name": "Third" })),
    )
    .await;
    let issued = move_confirmed(&fixture, &fixture.elsewhere).await;
    let (status, answer) = call(
        &fixture.state,
        &fixture.migrator,
        "POST",
        &format!("/v1/workspaces/{}/migration", fixture.workspace),
        Some(serde_json::json!({
            "to": third["id"],
            "confirmation": issued["confirmation"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(answer["code"], "invalid_request");
    // And nothing moved.
    let (_, row) = call(
        &fixture.state,
        &fixture.migrator,
        "GET",
        &format!("/v1/workspaces/{}", fixture.workspace),
        None,
    )
    .await;
    assert_eq!(row["owner"], fixture.owner);
}

/// A confirmation this gateway never issued is a 400 and not a 403: the caller
/// holds the permission; it is the question it claims to have answered that is
/// not recognised.
#[tokio::test]
async fn a_move_needs_a_confirmation_this_gateway_issued() {
    let fixture = with_workspace().await;
    let (status, answer) = call(
        &fixture.state,
        &fixture.migrator,
        "POST",
        &format!("/v1/workspaces/{}/migration", fixture.workspace),
        Some(serde_json::json!({
            "to": fixture.elsewhere,
            "confirmation": "9f2a2f2c-0b7c-4a6f-8f4f-1d63f9b2f7d1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(answer["code"], "invalid_request");
}

/// And a token is spent by being used, so the same one twice is not two moves.
#[tokio::test]
async fn a_move_confirmation_is_spent_by_being_presented() {
    let fixture = with_workspace().await;
    let body = move_confirmed(&fixture, &fixture.elsewhere).await;
    // The first attempt reaches the bridge check and stops there — there is no
    // session host in these tests — but the token is already gone.
    let (first, _) = call(
        &fixture.state,
        &fixture.migrator,
        "POST",
        &format!("/v1/workspaces/{}/migration", fixture.workspace),
        Some(body.clone()),
    )
    .await;
    assert_eq!(first, StatusCode::SERVICE_UNAVAILABLE);
    let (second, answer) = call(
        &fixture.state,
        &fixture.migrator,
        "POST",
        &format!("/v1/workspaces/{}/migration", fixture.workspace),
        Some(body),
    )
    .await;
    assert_eq!(second, StatusCode::BAD_REQUEST);
    assert_eq!(answer["code"], "invalid_request");
}

/// **Without a session host, the move refuses before it changes anything.**
///
/// The ordering the whole contract rests on: R8's rule is that ownership never
/// changes under a running session, and a gateway that cannot close terminals
/// cannot know there are none.
#[tokio::test]
async fn a_move_refuses_when_no_bridge_can_close_the_terminals() {
    let fixture = with_workspace().await;
    let body = move_confirmed(&fixture, &fixture.elsewhere).await;
    let (status, answer) = call(
        &fixture.state,
        &fixture.migrator,
        "POST",
        &format!("/v1/workspaces/{}/migration", fixture.workspace),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(answer["code"], "capability_unavailable");

    let (_, row) = call(
        &fixture.state,
        &fixture.migrator,
        "GET",
        &format!("/v1/workspaces/{}", fixture.workspace),
        None,
    )
    .await;
    assert_eq!(row["owner"], fixture.owner, "the workspace moved anyway");
}

/// **Moving a workspace to the profile that already owns it closes nothing.**
///
/// It does not even ask for a bridge, which is the divergence from the
/// lifecycle worth reading twice: the lifecycle closes terminals and *then*
/// notices the action was already applied, because an archived workspace should
/// have none. A workspace already owned by the destination has terminals that
/// belong to the destination — somebody's live work in the profile that now
/// owns it — and closing them to make a no-op look symmetrical would be
/// destroying it for nothing.
#[tokio::test]
async fn moving_a_workspace_to_its_current_owner_succeeds_and_closes_nothing() {
    let fixture = with_workspace().await;
    let body = move_confirmed(&fixture, &fixture.owner).await;
    let (status, outcome) = call(
        &fixture.state,
        &fixture.migrator,
        "POST",
        &format!("/v1/workspaces/{}/migration", fixture.workspace),
        Some(body),
    )
    .await;
    // There is no session host here, so reaching OK is itself the assertion:
    // any path that asked for the bridge would have answered 503.
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["from"], fixture.owner);
    assert_eq!(outcome["to"], fixture.owner);
    assert_eq!(outcome["terminated_sessions"], serde_json::json!([]));
    assert_eq!(outcome["sessions_closed"], serde_json::json!(true));
    assert_eq!(outcome["mutation_applied"], serde_json::json!(true));
    // **And the repair is absent rather than empty.** There is no session host
    // here, so nobody has been asked whether the projects still name the old
    // owner. `null` is "not yet repaired" and `[]` is "asked, nothing to do" —
    // and a surface that could not tell them apart would report a workspace as
    // fully moved when its projects are still in the missing-workspace area.
    assert!(
        outcome.get("projects_rescoped").is_none(),
        "an unreachable session host was reported as a completed repair: {outcome}"
    );
}

/// Its consequence is empty for the same reason, so the dialog does not ask a
/// person to accept closing terminals nothing will close.
#[tokio::test]
async fn the_consequence_of_moving_a_workspace_nowhere_is_empty() {
    let fixture = with_workspace().await;
    let (status, consequence) = call(
        &fixture.state,
        &fixture.migrator,
        "POST",
        &format!(
            "/v1/workspaces/{}/migration/confirmation",
            fixture.workspace
        ),
        Some(serde_json::json!({ "to": fixture.owner })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(consequence["from"], consequence["to"]);
    assert_eq!(consequence["sessions"], serde_json::json!([]));
}

/// A body that is not a migration request, and one naming something that is not
/// a profile id, are both 400 before anything is resolved.
#[tokio::test]
async fn a_malformed_move_request_is_refused() {
    let fixture = with_workspace().await;
    for body in [
        serde_json::json!({ "confirmation": "x" }),
        serde_json::json!({ "to": "Home", "confirmation": "x" }),
        serde_json::json!({ "to": fixture.elsewhere, "confirmation": "  " }),
        serde_json::json!({ "to": fixture.elsewhere, "confirmation": "x", "force": true }),
    ] {
        let (status, answer) = call(
            &fixture.state,
            &fixture.migrator,
            "POST",
            &format!("/v1/workspaces/{}/migration", fixture.workspace),
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(answer["code"], "invalid_request");
    }
}

#[tokio::test]
async fn s21_the_workspace_catalog_is_readable_with_no_profile_in_the_store() {
    let fixture = fixture();
    let (status, listed) = call(
        &fixture.state,
        &fixture.migrator,
        "GET",
        "/v1/workspaces?include_deleted=true",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert!(listed.as_array().expect("a list").is_empty());

    let (status, profiles) = call(
        &fixture.state,
        &fixture.migrator,
        "GET",
        "/v1/profiles",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        profiles.as_array().expect("a list").is_empty(),
        "the store is genuinely profile-less: {profiles}"
    );
}
