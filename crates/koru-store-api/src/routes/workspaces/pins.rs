// SPDX-License-Identifier: MPL-2.0

use super::*;

pub async fn set_pinned(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(workspace): Path<WorkspaceId>,
    Json(body): Json<koru_store_wire::WorkspacePin>,
) -> ApiResult<Json<koru_store_wire::PinnedWorkspace>> {
    caller.require(Scope::ManageWorkspaces)?;
    let outcome = lock(&state)?.set_workspace_pinned(workspace, body.pinned, now())?;
    // A pending listing still represents a committed manifest change.
    state.notify_replication();
    state.notify_catalog_change();
    state.publish_catalog_event(koru_events::WorkspaceCatalogAppearanceChanged {
        workspace_id: workspace.as_uuid(),
    });
    Ok(Json(outcome))
}

pub async fn reconcile_listing(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(workspace): Path<WorkspaceId>,
) -> ApiResult<Json<Workspace>> {
    caller.require(Scope::ManageWorkspaces)?;
    let row = lock(&state)?.reconcile_workspace_listing(workspace, now())?;
    state.notify_replication();
    state.notify_catalog_change();
    Ok(Json(row))
}

#[cfg(test)]
mod tests;
