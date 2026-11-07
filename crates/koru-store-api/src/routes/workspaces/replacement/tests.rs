// SPDX-License-Identifier: MPL-2.0

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use koru_store_core::service::Store;
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use tower::ServiceExt;

fn fixture() -> (Arc<AppState>, Workspace, Profile) {
    let mut store = Store::open(
        SqliteCatalog::in_memory().unwrap(),
        SqliteBackend::in_memory(),
    )
    .unwrap();
    let owner = store
        .create_profile("Owner", None, None, Locale::default(), now())
        .unwrap();
    let source = store.create_workspace(owner.id, "Source", now()).unwrap();
    let target = store
        .create_profile("Target", None, None, Locale::default(), now())
        .unwrap();
    (
        Arc::new(AppState::new(
            store,
            "<memory>".into(),
            "replacement-test".into(),
            42,
        )),
        source,
        target,
    )
}

async fn call(
    state: &Arc<AppState>,
    source: WorkspaceId,
    route: &str,
    body: serde_json::Value,
    scopes: Vec<Scope>,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::post(format!("/v1/workspaces/{source}/{route}-replacing"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(Caller {
        scopes,
        label: "test".into(),
    });
    let response = crate::router_for_trusted_requests(state.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let body =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    (status, body)
}

#[tokio::test]
async fn replacing_routes_require_the_verb_only_and_publish_localized_durable_replacement() {
    for (route, scope) in [
        ("archive", Scope::DestroyWorkspaces),
        ("delete", Scope::DestroyWorkspaces),
        ("move", Scope::MigrateWorkspaces),
    ] {
        let (state, source, target) = fixture();
        let changed = state.subscribe_catalog_changes();
        let (status, body) = call(
            &state,
            source.id,
            route,
            serde_json::json!({"name": "Arbeitsbereich", "owner": target.id}),
            vec![scope],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["replacement"]["state"], "created");
        assert_eq!(body["replacement"]["workspace"]["name"], "Arbeitsbereich");
        assert_eq!(
            body["replacement"]["workspace"]["owner"],
            source.owner.to_storage()
        );
        assert!(changed.has_changed().unwrap());
        let mut store = state.store.lock().unwrap();
        let rows = store.workspaces_including_deleted().unwrap();
        assert_eq!(rows.len(), 2);
        let replacement = rows.into_iter().find(|row| row.id != source.id).unwrap();
        assert!(store.manifest_id(replacement.id).is_ok());
    }
}

#[tokio::test]
async fn ordinary_management_cannot_remove_or_trigger_replacement() {
    for route in ["archive", "delete", "move"] {
        let (state, source, target) = fixture();
        let changed = state.subscribe_catalog_changes();
        let (status, _) = call(
            &state,
            source.id,
            route,
            serde_json::json!({"name":"Replacement", "owner":target.id}),
            vec![Scope::ManageWorkspaces],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(!changed.has_changed().unwrap());
        let store = state.store.lock().unwrap();
        assert_eq!(store.workspaces_including_deleted().unwrap(), vec![source]);
    }
}

#[tokio::test]
async fn delete_force_closes_rooms_and_retry_can_read_retained_trash() {
    let (state, source, _) = fixture();
    let room = crate::rooms::Room::Manifest(source.id);
    let who = state.rooms.next_connection();
    let (frames, _) = tokio::sync::mpsc::unbounded_channel();
    let (closer, mut closed) = tokio::sync::mpsc::unbounded_channel();
    state.rooms.register(who, closer);
    assert!(state.rooms.join(room.clone(), who, frames.clone()));
    let (status, _) = call(
        &state,
        source.id,
        "delete",
        serde_json::json!({"name":"Replacement"}),
        vec![Scope::DestroyWorkspaces],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(closed.try_recv().is_err());
    let (status, body) = call(
        &state,
        source.id,
        "delete",
        serde_json::json!({"name":"Replacement", "force":true}),
        vec![Scope::DestroyWorkspaces],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(closed.try_recv().is_ok());
    assert!(!state.rooms.join(room, who, frames));
    let replacement = body["replacement"]["workspace"]["id"].clone();
    let (status, body) = call(
        &state,
        source.id,
        "delete",
        serde_json::json!({"name":"Replacement", "force":true}),
        vec![Scope::DestroyWorkspaces],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["replacement"]["state"], "existing");
    assert_eq!(body["replacement"]["workspace"]["id"], replacement);
}

#[tokio::test]
async fn committed_authority_with_pending_listing_is_a_non_success_with_original_cause() {
    let (_, workspace, _) = fixture();
    let outcome = RemovedWorkspace {
        workspace,
        authority_committed: true,
        listing: ListingState::Pending {
            cause: "storage_failed".into(),
        },
        replacement: Replacement::Unknown,
    };
    let response = removal_response(outcome.clone());
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let failure: koru_store_wire::ListingPendingFailure =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(failure.code, "listing_pending");
    assert_eq!(failure.cause, "storage_failed");
    assert_eq!(failure.outcome, outcome);
}
