// SPDX-License-Identifier: MPL-2.0

use super::*;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use koru_store_wire::{
    ListingState, MoveWorkspaceReplacing, RemoveWorkspaceReplacing, RemovedWorkspace, Replacement,
};

pub async fn archive_replacing(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<RemoveWorkspaceReplacing>,
) -> ApiResult<Response> {
    caller.require(Scope::DestroyWorkspaces)?;
    if body.removal_attempt.is_some() {
        return Err(ApiError::bare(Code::InvalidRequest));
    }
    let outcome = lock(&state)?.archive_workspace_replacing(id, &body.name, now())?;
    publish_replacement(&state, &outcome.replacement);
    state.notify_replication();
    state.notify_catalog_change();
    state.publish_catalog_event(koru_events::WorkspaceCatalogArchived {
        workspace_id: id.as_uuid(),
    });
    Ok(removal_response(outcome))
}

pub async fn delete_replacing(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<RemoveWorkspaceReplacing>,
) -> ApiResult<Response> {
    caller.require(Scope::DestroyWorkspaces)?;
    let mut store = lock(&state)?;
    store.validate_workspace_removal(id, body.removal_attempt)?;
    let mut rooms = vec![crate::rooms::Room::Manifest(id)];
    // Retry includes retained Trash and never opens it just to enumerate rooms.
    match store.inspect_workspace_manifest(id)? {
        koru_store_core::service::WorkspaceManifestInspection::Verified(authority) => {
            rooms.extend(
                authority
                    .documents
                    .into_iter()
                    .filter(|id| *id != authority.manifest)
                    .map(crate::rooms::Room::Document),
            );
        }
        koru_store_core::service::WorkspaceManifestInspection::Unavailable(error) => {
            return Err(error.into());
        }
        _ => {
            return Err(ApiError::new(
                Code::CorruptState,
                "workspace authority is not verified",
            ));
        }
    }
    if state.rooms.occupants(&rooms) > 0 && !body.force {
        return Err(ApiError::bare(Code::WorkspaceInUse));
    }
    state.rooms.block_and_disconnect(&rooms);
    let result = match body.removal_attempt {
        Some(attempt) => store.delete_workspace_for_removal(id, attempt, now()),
        None => store.delete_workspace_replacing(id, &body.name, now()),
    };
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            state.rooms.unblock(&rooms);
            return Err(error.into());
        }
    };
    drop(store);
    // A listing failure is still a durable deletion: rooms stay blocked and
    // readers must refresh authority, including on another window's retry.
    publish_replacement(&state, &outcome.replacement);
    state.notify_replication();
    state.notify_catalog_change();
    state.publish_catalog_event(koru_events::WorkspaceCatalogDeleted {
        workspace_id: id.as_uuid(),
    });
    Ok(removal_response(outcome))
}

pub async fn move_replacing(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<MoveWorkspaceReplacing>,
) -> ApiResult<Json<koru_store_wire::MovedWorkspace>> {
    caller.require(Scope::MigrateWorkspaces)?;
    let outcome = lock(&state)?.move_workspace_replacing(id, body.owner, &body.name, now())?;
    publish_replacement(&state, &outcome.replacement);
    state.notify_replication();
    if outcome.from != outcome.workspace.owner {
        state.notify_catalog_change();
        state.publish_catalog_event(koru_events::WorkspaceCatalogMoved {
            workspace_id: id.as_uuid(),
            from_profile: outcome.from.as_uuid(),
            to_profile: outcome.workspace.owner.as_uuid(),
        });
    }
    Ok(Json(outcome))
}

fn publish_replacement(state: &AppState, replacement: &Replacement) {
    if let Replacement::Created(workspace) = replacement {
        state.publish_catalog_event(koru_events::WorkspaceCatalogCreated {
            workspace_id: workspace.id.as_uuid(),
        });
    }
}

fn removal_response(outcome: RemovedWorkspace) -> Response {
    if let ListingState::Pending { cause } = &outcome.listing {
        (
            StatusCode::CONFLICT,
            Json(koru_store_wire::ListingPendingFailure {
                code: "listing_pending".into(),
                message: "workspace authority committed; catalog publication is pending".into(),
                cause: cause.clone(),
                outcome,
            }),
        )
            .into_response()
    } else {
        Json(outcome).into_response()
    }
}

#[cfg(test)]
mod tests;
