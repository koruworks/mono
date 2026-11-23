// SPDX-License-Identifier: MPL-2.0

//! Workspaces that are in the Trash on this device while the account still
//! holds them live (`docs/features/trash-recovery/PLAN.final.md` P2, P4).
//!
//! | Route | Store call |
//! |---|---|
//! | `GET /v1/workspaces/trashed-in-account` | `Store::trashed_in_account` |
//! | `POST /v1/workspaces/{id}/trashed-in-account/acknowledge` | `Store::acknowledge_trashed_in_account` |
//!
//! The way back is the neighbouring `POST /v1/workspaces/{id}/restore-to`,
//! with the Profile the listing names. Nothing here restores anything.

use super::*;
use koru_store_wire::TrashedInAccount;

/// Each such workspace, with the Profile the account keeps it under.
pub async fn list(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<Vec<TrashedInAccount>>> {
    caller.require(Scope::ManageWorkspaces)?;
    Ok(Json(lock(&state)?.trashed_in_account()?))
}

/// Record that the person was told about one, so they are told once on this
/// device. Marking one that is not listed is not an error: the notice it
/// answers may have been about a workspace that has since been restored.
pub async fn acknowledge(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<Json<()>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = WorkspaceId::parse(&id).ok_or_else(invalid_id)?;
    let marked = lock(&state)?.acknowledge_trashed_in_account(id, Timestamp::now())?;
    if marked {
        // Every window reads the listing: the others stop offering the
        // notice this one has shown.
        state.notify_catalog_change();
        state.publish_catalog_event(koru_events::WorkspaceTrashedInAccountChanged);
    }
    Ok(Json(()))
}

#[cfg(test)]
mod tests;
