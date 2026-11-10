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

/// The verb's own scope is the whole grant, and nothing is manufactured for
/// the Profile the removal empties (root-catalog Plan 1.2; STORE.md "Removal
/// with a required replacement"). The `name` the routes still accept is
/// unused, so there is no replacement to name in any language.
#[tokio::test]
async fn replacing_routes_require_the_verb_only_and_manufacture_no_replacement() {
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
        // The source Profile held only this workspace, so it is left empty.
        assert_eq!(
            body["replacement"]["state"], "not_needed",
            "{route}: {body}"
        );
        assert!(
            body["replacement"]["workspace"].is_null(),
            "{route}: {body}"
        );
        assert!(changed.has_changed().unwrap());
        let store = state.store.lock().unwrap();
        let rows = store.workspaces_including_deleted().unwrap();
        assert_eq!(rows.len(), 1, "{route}: a workspace was manufactured");
        assert_eq!(rows[0].id, source.id);
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

/// Two removals racing for the same Profile's last two workspaces: both
/// lifecycle commits land, nothing is manufactured for the Profile they
/// empty, and whatever either one reports as a replacement is a workspace
/// that was really live when it looked (root-catalog Plan 1.2). Which one
/// still sees its sibling depends on who takes the store first, so the test
/// pins the honesty of both answers rather than a particular one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn competing_removals_publish_no_replacement_and_empty_the_owner() {
    for (route, scope) in [
        ("archive", Scope::DestroyWorkspaces),
        ("delete", Scope::DestroyWorkspaces),
        ("move", Scope::MigrateWorkspaces),
    ] {
        let (state, first, target) = fixture();
        let second = state
            .store
            .lock()
            .unwrap()
            .create_workspace(first.owner, "Second", now())
            .unwrap();
        let ready = Arc::new(tokio::sync::Barrier::new(3));
        let mut requests = Vec::new();
        for source in [first.id, second.id] {
            let state = state.clone();
            let ready = ready.clone();
            requests.push(tokio::spawn(async move {
                ready.wait().await;
                call(
                    &state,
                    source,
                    route,
                    serde_json::json!({"name":"Replacement", "owner":target.id}),
                    vec![scope],
                )
                .await
            }));
        }
        ready.wait().await;
        let live = [first.id.to_storage(), second.id.to_storage()];
        for request in requests {
            let (status, body) = request.await.unwrap();
            assert_eq!(status, StatusCode::OK, "{route}: {body}");
            let answer = body["replacement"]["state"].clone();
            match answer.as_str() {
                // The sibling was live when this operation looked at it, even
                // if the competing removal retired it a moment later.
                Some("existing") => {
                    let selected = body["replacement"]["workspace"]["id"].clone();
                    assert!(
                        live.iter().any(|id| selected == id.as_str()),
                        "{route}: selected a workspace nobody created: {body}"
                    );
                }
                // The competitor got there first and the Profile is empty.
                Some("not_needed") => {}
                _ => panic!("{route}: no replacement is ever manufactured: {body}"),
            }
        }
        let store = state.store.lock().unwrap();
        let rows = store.workspaces_including_deleted().unwrap();
        let mut ids: Vec<_> = rows.iter().map(|row| row.id).collect();
        ids.sort_unstable();
        let mut expected = vec![first.id, second.id];
        expected.sort_unstable();
        assert_eq!(ids, expected, "{route}: a workspace was manufactured");
        // Both lifecycle commits landed, and an emptied Profile is a valid
        // state: nobody creates one back.
        let remaining: Vec<_> = rows
            .iter()
            .filter(|row| row.owner == first.owner && !row.archived && row.deleted_at.is_none())
            .collect();
        assert!(
            remaining.is_empty(),
            "{route}: a removal did not commit: {remaining:?}"
        );
    }
}

/// The source Profile keeps a second live workspace, so the forced delete
/// and its retry both have one to **select** (`Replacement::Existing`): the
/// only route-level cover for selection, and what makes the retry's answer
/// comparable with the first one's.
#[tokio::test]
async fn delete_force_closes_rooms_and_retry_can_read_retained_trash() {
    let (state, source, _) = fixture();
    let sibling = state
        .store
        .lock()
        .unwrap()
        .create_workspace(source.owner, "Sibling", now())
        .unwrap();
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
    assert_eq!(body["replacement"]["state"], "existing", "{body}");
    assert_eq!(
        body["replacement"]["workspace"]["id"],
        sibling.id.to_storage()
    );
    let (status, body) = call(
        &state,
        source.id,
        "delete",
        serde_json::json!({"name":"Replacement", "force":true}),
        vec![Scope::DestroyWorkspaces],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The retry reads the workspace retained in Trash and selects the same
    // live sibling again; nothing is created for either answer.
    assert_eq!(body["workspace"]["id"], source.id.to_storage(), "{body}");
    assert!(!body["workspace"]["deleted_at"].is_null(), "{body}");
    assert_eq!(body["replacement"]["state"], "existing");
    assert_eq!(
        body["replacement"]["workspace"]["id"],
        sibling.id.to_storage()
    );
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
