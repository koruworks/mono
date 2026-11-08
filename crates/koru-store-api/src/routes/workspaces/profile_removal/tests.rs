// SPDX-License-Identifier: MPL-2.0

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use koru_store_core::service::Store;
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use tower::ServiceExt;

fn fixture() -> (Arc<AppState>, Workspace) {
    let mut store = Store::open(
        SqliteCatalog::in_memory().unwrap(),
        SqliteBackend::in_memory(),
    )
    .unwrap();
    let owner = store
        .create_profile("Owner", None, None, Locale::default(), now())
        .unwrap();
    let workspace = store.create_workspace(owner.id, "Content", now()).unwrap();
    (
        Arc::new(AppState::new(
            store,
            "<memory>".into(),
            "removal-test".into(),
            42,
        )),
        workspace,
    )
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
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn removal_routes_preserve_scope_and_exact_attempt_and_keep_is_not_restore() {
    let (state, source) = fixture();
    let path = format!("/v1/profiles/{}/removal", source.owner);
    let (status, _) = call(
        &state,
        "POST",
        &path,
        Some(serde_json::json!({"kind":"local"})),
        Scope::ManageWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, absent) = call(&state, "GET", &path, None, Scope::ReadDocuments).await;
    assert_eq!(status, StatusCode::OK);
    assert!(absent.is_null());
    let (status, mark) = call(
        &state,
        "POST",
        &path,
        Some(serde_json::json!({"kind":"local"})),
        Scope::DestroyWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{mark}");
    let (status, failure) = call(
        &state,
        "POST",
        &path,
        Some(serde_json::json!({"kind":"local"})),
        Scope::DestroyWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(failure["code"], "removal_in_progress");
    let body = serde_json::json!({"attempt": mark["attempt"]});
    let (_, stopped) = call(
        &state,
        "POST",
        &format!("{path}/stop"),
        Some(body.clone()),
        Scope::DestroyWorkspaces,
    )
    .await;
    assert!(stopped.is_null());
    let (status, _) = call(
        &state,
        "DELETE",
        &path,
        Some(body.clone()),
        Scope::DestroyWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, failure) = call(
        &state,
        "POST",
        &format!("{path}/stop"),
        Some(body),
        Scope::DestroyWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(failure["code"], "removal_attempt_stale");
    assert!(lock(&state).unwrap().workspace(source.id).is_ok());
}

#[tokio::test]
async fn stale_step_keeps_rooms_open_and_current_cascade_creates_no_replacement() {
    let (state, source) = fixture();
    let path = format!("/v1/profiles/{}/removal", source.owner);
    let (_, mark) = call(
        &state,
        "POST",
        &path,
        Some(serde_json::json!({"kind":"local"})),
        Scope::DestroyWorkspaces,
    )
    .await;
    let room = crate::rooms::Room::Manifest(source.id);
    let who = state.rooms.next_connection();
    let (frames, _) = tokio::sync::mpsc::unbounded_channel();
    let (closer, mut closed) = tokio::sync::mpsc::unbounded_channel();
    state.rooms.register(who, closer);
    assert!(state.rooms.join(room, who, frames));
    let deletion = format!("/v1/workspaces/{}/delete-replacing", source.id);
    let (status, failure) = call(
        &state,
        "POST",
        &deletion,
        Some(serde_json::json!({"name":"Default","force":true})),
        Scope::DestroyWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(failure["code"], "profile_being_removed");
    assert!(closed.try_recv().is_err());
    let (status, result) = call(
        &state,
        "POST",
        &deletion,
        Some(serde_json::json!({"name":"", "force":true,"removal_attempt":mark["attempt"]})),
        Scope::DestroyWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["replacement"]["state"], "not_needed");
    assert!(closed.try_recv().is_ok());
    let completion = format!("/v1/profiles/{}/local", source.owner);
    let (status, failure) =
        call(&state, "DELETE", &completion, None, Scope::ManageWorkspaces).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(failure["code"], "removal_not_begun");
    let (status, _) = call(
        &state,
        "DELETE",
        &completion,
        Some(serde_json::json!({"attempt":mark["attempt"]})),
        Scope::ManageWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let store = lock(&state).unwrap();
    assert!(store.profiles().unwrap().is_empty());
    assert!(store.profile_removal(source.owner).unwrap().is_none());
    assert_eq!(store.workspaces_including_deleted().unwrap().len(), 1);
}
