// SPDX-License-Identifier: MPL-2.0

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use koru_store_core::{Locale, service::Store};
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use tower::ServiceExt;

#[tokio::test]
async fn narrow_pin_and_explicit_repair_require_management_before_reading_any_identity() {
    let mut store = Store::open(
        SqliteCatalog::in_memory().unwrap(),
        SqliteBackend::in_memory(),
    )
    .unwrap();
    let owner = store
        .create_profile("Owner", None, None, Locale::default(), now())
        .unwrap();
    let workspace = store.create_workspace(owner.id, "Pinned", now()).unwrap();
    let state = Arc::new(AppState::new(
        store,
        "<memory>".into(),
        "pins-test".into(),
        42,
    ));
    let app = crate::router_for_trusted_requests(state.clone());
    for (method, route) in [("PUT", "pinned"), ("POST", "reconcile-listing")] {
        for (id, scope, expected) in [
            (
                WorkspaceId::new(),
                Scope::ReadDocuments,
                StatusCode::FORBIDDEN,
            ),
            (workspace.id, Scope::ReadDocuments, StatusCode::FORBIDDEN),
            (workspace.id, Scope::ManageWorkspaces, StatusCode::OK),
        ] {
            let mut request = Request::builder()
                .method(method)
                .uri(format!("/v1/workspaces/{id}/{route}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"pinned":true}"#))
                .unwrap();
            request.extensions_mut().insert(Caller {
                scopes: vec![scope],
                label: "test".into(),
            });
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), expected);
            if expected == StatusCode::OK && route == "pinned" {
                let outcome: koru_store_wire::PinnedWorkspace = serde_json::from_slice(
                    &to_bytes(response.into_body(), usize::MAX).await.unwrap(),
                )
                .unwrap();
                assert!(outcome.workspace.pinned);
                assert_eq!(
                    outcome.listing,
                    koru_store_core::service::ListingState::Applied
                );
            }
        }
    }
    assert!(
        state
            .store
            .lock()
            .unwrap()
            .workspace(workspace.id)
            .unwrap()
            .pinned
    );
}
