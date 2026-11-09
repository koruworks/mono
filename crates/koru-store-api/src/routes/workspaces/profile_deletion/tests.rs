// SPDX-License-Identifier: MPL-2.0

//! The batch over the wire, against an in-memory Store: preview → delete →
//! the restorable listing → restore, the stale-preview refusal, the scopes,
//! and the events each commit publishes.

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use koru_store_core::service::Store;
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use std::sync::Mutex;
use tower::ServiceExt;

/// Every event the routes published, by name, in order.
#[derive(Default)]
struct Recorded(Mutex<Vec<String>>);

impl crate::catalog_events::CatalogEventSink for Recorded {
    fn publish(&self, event: koru_events::Draft) {
        // `Draft` keeps its name private; its `Debug` names it, which is all
        // this needs to know which fact was published.
        self.0.lock().unwrap().push(format!("{event:?}"));
    }
}

fn fixture() -> (Arc<AppState>, Arc<Recorded>, Profile, Workspace) {
    let mut store = Store::open(
        SqliteCatalog::in_memory().unwrap(),
        SqliteBackend::in_memory(),
    )
    .unwrap();
    let owner = store
        .create_profile("Owner", None, None, Locale::default(), now())
        .unwrap();
    let workspace = store.create_workspace(owner.id, "Content", now()).unwrap();
    let state = Arc::new(AppState::new(
        store,
        "<memory>".into(),
        "deletion-test".into(),
        42,
    ));
    let recorded = Arc::new(Recorded::default());
    assert!(state.set_catalog_event_sink(recorded.clone()));
    (state, recorded, owner, workspace)
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

fn published(recorded: &Recorded, name: &str) -> usize {
    recorded
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.contains(&format!("\"{name}\"")))
        .count()
}

#[tokio::test]
async fn preview_delete_list_and_restore_round_trip_with_their_events() {
    let (state, recorded, owner, first) = fixture();
    let preview_path = format!("/v1/profiles/{}/deletion-preview", owner.id);

    // The reads take manage_workspaces; a document reader learns nothing.
    let (status, _) = call(&state, "GET", &preview_path, None, Scope::ReadDocuments).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, preview) = call(&state, "GET", &preview_path, None, Scope::ManageWorkspaces).await;
    assert_eq!(status, StatusCode::OK);
    let preview: ProfileDeletionPreview = serde_json::from_value(preview).unwrap();
    assert_eq!(preview.targets, vec![first.id]);

    // The batch takes destroy_workspaces, like the workspace delete.
    let delete_path = format!("/v1/profiles/{}/delete", owner.id);
    let (status, _) = call(
        &state,
        "POST",
        &delete_path,
        Some(
            serde_json::to_value(DeleteProfile {
                preview: preview.clone(),
            })
            .unwrap(),
        ),
        Scope::ManageWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A second workspace appears after the preview was shown: the
    // confirmation is stale and the Store says so with a conflict.
    let second = lock(&state)
        .unwrap()
        .create_workspace(owner.id, "Later", now())
        .unwrap();
    let (status, refusal) = call(
        &state,
        "POST",
        &delete_path,
        Some(serde_json::to_value(DeleteProfile { preview }).unwrap()),
        Scope::DestroyWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(refusal["code"], "idempotency_conflict");
    assert_eq!(published(&recorded, "profile.deleted"), 0);

    // Refreshed, the preview names both and the batch commits.
    let (_, preview) = call(&state, "GET", &preview_path, None, Scope::ManageWorkspaces).await;
    let preview: ProfileDeletionPreview = serde_json::from_value(preview).unwrap();
    assert_eq!(preview.targets.len(), 2);
    let (status, batch) = call(
        &state,
        "POST",
        &delete_path,
        Some(serde_json::to_value(DeleteProfile { preview }).unwrap()),
        Scope::DestroyWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{batch}");
    let batch: ProfileDeletionBatch = serde_json::from_value(batch).unwrap();
    assert_eq!(batch.targets.len(), 2);
    assert_eq!(published(&recorded, "profile.deleted"), 1);
    assert_eq!(published(&recorded, "workspace.catalog.deleted"), 2);

    // The Profile left the ordinary listing and is in the restorable one,
    // by name, with its captured targets and nothing expired yet.
    let (_, profiles) = call(&state, "GET", "/v1/profiles", None, Scope::ReadDocuments).await;
    assert!(profiles.as_array().unwrap().is_empty());
    let (status, deleted) = call(
        &state,
        "GET",
        "/v1/profiles/deleted",
        None,
        Scope::ManageWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let deleted: Vec<DeletedProfileListing> = serde_json::from_value(deleted).unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].name.as_deref(), Some("Owner"));
    assert_eq!(deleted[0].deletion.profile, owner.id);
    assert_eq!(deleted[0].deletion.targets.len(), 2);
    assert!(deleted[0].deletion.expired_or_purged.is_empty());

    // Nothing is unassigned: the captured targets are in the Trash, not
    // live under a deleted Profile.
    let (status, unassigned) = call(
        &state,
        "GET",
        "/v1/workspaces/unassigned",
        None,
        Scope::ManageWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(unassigned.as_array().unwrap().is_empty());

    // The restore observes exactly that batch and brings both back.
    let restore_path = format!("/v1/profiles/{}/restore", owner.id);
    let body = serde_json::to_value(RestoreProfile {
        deletion: deleted[0].deletion.deletion,
    })
    .unwrap();
    let (status, _) = call(
        &state,
        "POST",
        &restore_path,
        Some(body.clone()),
        Scope::ManageWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, outcome) = call(
        &state,
        "POST",
        &restore_path,
        Some(body),
        Scope::DestroyWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    let outcome: ProfileRestoreOutcome = serde_json::from_value(outcome).unwrap();
    let mut restored = outcome.restored.clone();
    restored.sort();
    let mut expected = vec![first.id, second.id];
    expected.sort();
    assert_eq!(restored, expected);
    assert!(outcome.left_in_trash.is_empty());
    assert!(outcome.purged.is_empty());
    assert_eq!(published(&recorded, "profile.restored"), 1);
    assert_eq!(published(&recorded, "workspace.catalog.restored"), 2);

    let (_, profiles) = call(&state, "GET", "/v1/profiles", None, Scope::ReadDocuments).await;
    assert_eq!(profiles.as_array().unwrap().len(), 1);
    let (_, deleted) = call(
        &state,
        "GET",
        "/v1/profiles/deleted",
        None,
        Scope::ManageWorkspaces,
    )
    .await;
    assert!(deleted.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn an_earlier_trash_entry_stays_trashed_through_a_batch_restore() {
    let (state, _, owner, first) = fixture();
    // `first` is trashed on its own before the Profile goes; the batch
    // captures only what is live.
    let second = lock(&state)
        .unwrap()
        .create_workspace(owner.id, "Live", now())
        .unwrap();
    lock(&state)
        .unwrap()
        .delete_workspace(first.id, now())
        .unwrap();
    let (_, preview) = call(
        &state,
        "GET",
        &format!("/v1/profiles/{}/deletion-preview", owner.id),
        None,
        Scope::ManageWorkspaces,
    )
    .await;
    let preview: ProfileDeletionPreview = serde_json::from_value(preview).unwrap();
    assert_eq!(preview.targets, vec![second.id]);
    let (status, _) = call(
        &state,
        "POST",
        &format!("/v1/profiles/{}/delete", owner.id),
        Some(serde_json::to_value(DeleteProfile { preview }).unwrap()),
        Scope::DestroyWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, deleted) = call(
        &state,
        "GET",
        "/v1/profiles/deleted",
        None,
        Scope::ManageWorkspaces,
    )
    .await;
    let deleted: Vec<DeletedProfileListing> = serde_json::from_value(deleted).unwrap();
    let (status, outcome) = call(
        &state,
        "POST",
        &format!("/v1/profiles/{}/restore", owner.id),
        Some(
            serde_json::to_value(RestoreProfile {
                deletion: deleted[0].deletion.deletion,
            })
            .unwrap(),
        ),
        Scope::DestroyWorkspaces,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    let outcome: ProfileRestoreOutcome = serde_json::from_value(outcome).unwrap();
    assert_eq!(outcome.restored, vec![second.id]);
    let rows = lock(&state)
        .unwrap()
        .workspaces_including_deleted()
        .unwrap();
    let earlier = rows.iter().find(|row| row.id == first.id).unwrap();
    assert!(
        earlier.deleted_at.is_some(),
        "the earlier Trash entry stands"
    );
    let live = rows.iter().find(|row| row.id == second.id).unwrap();
    assert!(live.deleted_at.is_none());
}
