// SPDX-License-Identifier: MPL-2.0

use super::*;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use koru_store_core::Store;
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use tower::ServiceExt;

async fn request(
    state: Arc<AppState>,
    token: &str,
    method: &str,
    path: &str,
    value: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = crate::router(state)
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&value).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 100_000)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap_or_default())
}

#[tokio::test]
async fn sync_modes_routes_commit_wake_preserve_choices_and_refuse_stale_writes() {
    let mut store = Store::open(
        SqliteCatalog::in_memory().unwrap(),
        SqliteBackend::in_memory(),
    )
    .unwrap();
    let profile = store
        .create_profile("Test", None, None, Locale::default(), now())
        .unwrap()
        .id;
    let workspace = store
        .create_workspace(profile, "Workspace", now())
        .unwrap()
        .id;
    let state = Arc::new(AppState::new(
        store,
        "<memory>".into(),
        "sync-modes-test".into(),
        1,
    ));
    let (token, _) = state.mint_session("test", Scope::ALL.to_vec());
    let wake = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = wake.clone();
    state.set_replication_notifier(Arc::new(move || {
        count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }));
    let profile_path = format!("/v1/profiles/{profile}/transport-mode");
    let workspace_path = format!("/v1/workspaces/{workspace}/transport-mode");
    let (status, saved) = request(
        state.clone(),
        &token,
        "PUT",
        &profile_path,
        serde_json::json!({"mode":"p2p", "include_workspaces":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["mode"], "p2p");
    assert_eq!(wake.load(std::sync::atomic::Ordering::Relaxed), 1);
    let (_, settings) = request(
        state.clone(),
        &token,
        "GET",
        &profile_path,
        serde_json::Value::Null,
    )
    .await;
    let revision = settings["workspaces"][0]["revision"].as_i64().unwrap();
    let (status, _) = request(
        state.clone(),
        &token,
        "PUT",
        &workspace_path,
        serde_json::json!({"mode":"cloud", "expected_revision":revision}),
    )
    .await;
    assert!(!status.is_success(), "a child cannot exceed its profile");
    let (status, saved) = request(
        state.clone(),
        &token,
        "PUT",
        &workspace_path,
        serde_json::json!({"mode":"local_only", "expected_revision":revision}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["workspaces"][0]["mode"], "local_only");
    let (status, _) = request(
        state.clone(),
        &token,
        "PUT",
        &workspace_path,
        serde_json::json!({"mode":"p2p", "expected_revision":revision}),
    )
    .await;
    assert!(
        !status.is_success(),
        "stale write must not replace a choice"
    );
    let (_, saved) = request(
        state.clone(),
        &token,
        "POST",
        &format!("/v1/profiles/{profile}/transport-default"),
        serde_json::json!({"mode":"cloud", "include_workspaces":true}),
    )
    .await;
    assert_eq!(saved["mode"], "p2p");
    assert_eq!(saved["workspaces"][0]["mode"], "local_only");
    let (read_token, _) = state.mint_session("read", vec![Scope::ReadDocuments]);
    let (status, _) = request(
        state,
        &read_token,
        "PUT",
        &workspace_path,
        serde_json::json!({"mode":"p2p", "expected_revision":revision + 1}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
