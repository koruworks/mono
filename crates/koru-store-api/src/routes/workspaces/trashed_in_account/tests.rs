// SPDX-License-Identifier: MPL-2.0

//! The listing and its acknowledgement over the wire, against an in-memory
//! Store that holds one workspace in its Trash while an account lists it
//! live under another Profile here.

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use koru_store_core::service::Store;
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use tower::ServiceExt;

fn now() -> Timestamp {
    Timestamp::from_millis(1_800_000_000_000)
}

/// A workspace trashed under a Profile nobody opened to sync — its deletion
/// cannot leave this device — and a Profile of `account` that the account
/// keeps the workspace under.
fn fixture() -> (Arc<AppState>, WorkspaceId, ProfileId, uuid::Uuid) {
    let mut store = Store::open(
        SqliteCatalog::in_memory().unwrap(),
        SqliteBackend::in_memory(),
    )
    .unwrap();
    let quiet = store
        .create_profile("Quiet", None, None, Locale::default(), now())
        .unwrap();
    let workspace = store.create_workspace(quiet.id, "Stuck", now()).unwrap();
    store.delete_workspace(workspace.id, now()).unwrap();
    let kept = store
        .create_profile("Kept", None, None, Locale::default(), now())
        .unwrap();
    let account = uuid::Uuid::new_v4();
    let binding = ProfileAccountBinding::parse_identity_id(&account.to_string()).unwrap();
    store.bind_profile_account(kept.id, None, binding).unwrap();
    let outcome = store
        .reconcile_account_live(account, &[(workspace.id, kept.id)], now())
        .unwrap();
    assert!(outcome.changed, "the fixture: the fact is recorded");
    let state = Arc::new(AppState::new(
        store,
        "<memory>".into(),
        "trashed-in-account-test".into(),
        42,
    ));
    (state, workspace.id, kept.id, account)
}

async fn call(
    state: &Arc<AppState>,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    scope: Scope,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let mut request = request
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .unwrap();
    request.extensions_mut().insert(Caller {
        scopes: vec![scope],
        label: "test".into(),
    });
    let response = crate::router_for_trusted_requests(state.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

async fn listed(state: &Arc<AppState>) -> Vec<TrashedInAccount> {
    let (status, body) = call(
        state,
        "GET",
        "/v1/workspaces/trashed-in-account",
        None,
        Scope::ManageWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    serde_json::from_value(body).unwrap()
}

#[tokio::test]
async fn the_listing_names_the_workspace_and_the_profile_the_account_keeps_it_under() {
    let (state, workspace, kept, _) = fixture();

    let (status, _) = call(
        &state,
        "GET",
        "/v1/workspaces/trashed-in-account",
        None,
        Scope::ReadDocuments,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the scope the listing takes");

    let rows = listed(&state).await;
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].workspace.id, rows[0].profile), (workspace, kept));
    assert_eq!(rows[0].workspace.name, "Stuck");
    assert!(rows[0].workspace.deleted_at.is_some(), "still in the Trash");
    assert_eq!(rows[0].acknowledged_ms, None, "nobody has been told yet");
}

#[tokio::test]
async fn acknowledging_marks_it_once_and_wakes_the_other_readers() {
    let (state, workspace, _, _) = fixture();
    let mut changes = state.subscribe_catalog_changes();
    let _ = changes.borrow_and_update();
    let path = format!("/v1/workspaces/{workspace}/trashed-in-account/acknowledge");

    let (status, _) = call(&state, "POST", &path, None, Scope::ManageWorkspaces).await;
    assert_eq!(status, StatusCode::OK);
    assert!(changes.has_changed().unwrap(), "the listing changed");
    let first = listed(&state).await[0]
        .acknowledged_ms
        .expect("the person was told");

    // Told once: a second acknowledgement keeps the first time.
    let (status, _) = call(&state, "POST", &path, None, Scope::ManageWorkspaces).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed(&state).await[0].acknowledged_ms, Some(first));

    // A workspace that is not listed is not an error, and wakes nobody.
    let _ = changes.borrow_and_update();
    let other = format!(
        "/v1/workspaces/{}/trashed-in-account/acknowledge",
        WorkspaceId::new()
    );
    let (status, _) = call(&state, "POST", &other, None, Scope::ManageWorkspaces).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!changes.has_changed().unwrap());
}

#[tokio::test]
async fn restoring_it_into_the_profile_empties_the_listing() {
    let (state, workspace, kept, _) = fixture();
    let (status, restored) = call(
        &state,
        "POST",
        &format!("/v1/workspaces/{workspace}/restore-to"),
        Some(serde_json::json!({ "owner": kept })),
        Scope::DestroyWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let restored: Workspace = serde_json::from_value(restored).unwrap();
    assert_eq!((restored.owner, restored.deleted_at), (kept, None));
    assert!(listed(&state).await.is_empty());
}
