// SPDX-License-Identifier: MPL-2.0

use crate::{AppState, auth::Caller};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    routing::post,
};
use koru_store_core::{ProfileId, Scope, service::Store};
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use std::sync::Arc;
use tower::ServiceExt;

fn state() -> Arc<AppState> {
    Arc::new(AppState::new(
        Store::open(
            SqliteCatalog::in_memory().unwrap(),
            SqliteBackend::in_memory(),
        )
        .unwrap(),
        "<memory>".into(),
        "creation-test".into(),
        42,
    ))
}

fn request(id: ProfileId, scopes: Vec<Scope>) -> Request<Body> {
    let mut request = Request::post("/v1/profiles/with-workspace")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "id": id, "name": "Owner", "workspace": { "name": "First" }
            })
            .to_string(),
        ))
        .unwrap();
    request.extensions_mut().insert(Caller {
        scopes,
        label: "test".into(),
    });
    request
}

#[tokio::test]
async fn combined_route_publishes_both_and_exact_retry_keeps_workspace_identity() {
    let state = state();
    let app = crate::router_for_trusted_requests(state.clone());
    let id = ProfileId::new();
    let first = app
        .clone()
        .oneshot(request(id, vec![Scope::ManageWorkspaces]))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first: koru_store_wire::CreatedProfile =
        serde_json::from_slice(&to_bytes(first.into_body(), usize::MAX).await.unwrap()).unwrap();
    let second = app
        .oneshot(request(id, vec![Scope::ManageWorkspaces]))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second: koru_store_wire::CreatedProfile =
        serde_json::from_slice(&to_bytes(second.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(first.profile.id, id);
    assert_eq!(first.workspace.id, second.workspace.id);
    let store = state.store.lock().unwrap();
    assert_eq!(store.profiles().unwrap().len(), 1);
    assert_eq!(store.workspaces().unwrap().len(), 1);
}

#[tokio::test]
async fn old_route_table_refuses_new_body_without_creating_a_profile() {
    let state = state();
    let old = Router::new()
        .route("/v1/profiles", post(super::create_profile))
        .with_state(state.clone());
    let response = old
        .oneshot(request(ProfileId::new(), vec![Scope::ManageWorkspaces]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(state.store.lock().unwrap().profiles().unwrap().is_empty());
    assert!(state.store.lock().unwrap().workspaces().unwrap().is_empty());
}

#[tokio::test]
async fn permission_refusal_precedes_any_publication() {
    let state = state();
    let response = crate::router_for_trusted_requests(state.clone())
        .oneshot(request(ProfileId::new(), vec![Scope::ReadDocuments]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(state.store.lock().unwrap().profiles().unwrap().is_empty());
    assert!(state.store.lock().unwrap().workspaces().unwrap().is_empty());
}

fn selection_request(owner: ProfileId, scope: Scope, authority: bool) -> Request<Body> {
    let route = if authority {
        "authority"
    } else {
        "ensure-live"
    };
    let mut request = Request::builder()
        .method(if authority { "GET" } else { "POST" })
        .uri(format!("/v1/profiles/{owner}/workspaces/{route}"))
        .header("content-type", "application/json")
        .body(Body::from(if authority {
            ""
        } else {
            r#"{"name":"Default"}"#
        }))
        .unwrap();
    request.extensions_mut().insert(Caller {
        scopes: vec![scope],
        label: "test".into(),
    });
    request
}

#[tokio::test]
async fn selection_requires_management_but_authority_is_read_only() {
    let state = state();
    let owner = state
        .store
        .lock()
        .unwrap()
        .create_profile(
            "Owner",
            None,
            None,
            koru_store_core::Locale::default(),
            koru_store_core::Timestamp::now(),
        )
        .unwrap()
        .id;
    let app = crate::router_for_trusted_requests(state.clone());
    let refused = app
        .clone()
        .oneshot(selection_request(owner, Scope::ReadDocuments, false))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    let read = app
        .clone()
        .oneshot(selection_request(owner, Scope::ReadDocuments, true))
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
    let rows: Vec<koru_store_wire::WorkspaceAuthority> =
        serde_json::from_slice(&to_bytes(read.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(rows.is_empty());
    assert!(state.store.lock().unwrap().workspaces().unwrap().is_empty());
    for expected in ["created", "active"] {
        let response = app
            .clone()
            .oneshot(selection_request(owner, Scope::ManageWorkspaces, false))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["state"], expected);
    }
    assert_eq!(state.store.lock().unwrap().workspaces().unwrap().len(), 1);
}

#[tokio::test]
async fn authority_permission_is_checked_before_owner_existence() {
    let state = state();
    let response = crate::router_for_trusted_requests(state)
        .oneshot(selection_request(
            ProfileId::new(),
            Scope::ManageWorkspaces,
            true,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
