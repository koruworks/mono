// SPDX-License-Identifier: MPL-2.0

//! **The shared Profile deletion batch, its restore, and the recovery reads**
//! (root-catalog TECHNICAL.final §3.6, §3.7, §6.1, §6.3; `STORE.md`
//! "Lifecycle on the root").
//!
//! Beside, and distinct from, the device-local routes in `workspaces.rs`
//! (`DELETE`/`PUT /v1/profiles/{id}/local`) and the attempts in
//! [`super::profile_removal`]: those hide a Profile on **this installation**
//! and consent to nothing global. These write a **root fact** that converges
//! between a person's devices (M2) and is undone as a batch:
//!
//! - `GET /v1/profiles/{id}/deletion-preview` — what a deletion would trash
//!   right now, with the revision the confirmation echoes back;
//! - `POST /v1/profiles/{id}/delete` — the batch, refused with
//!   `idempotency_conflict` (409) when the preview is stale;
//! - `GET /v1/profiles/deleted` — the restorable deletions this device holds;
//! - `POST /v1/profiles/{id}/restore` — the batch restore and its honest
//!   per-target outcome;
//! - `GET /v1/workspaces/unassigned` — live Workspaces reachable from no
//!   active Profile, with the reason.
//!
//! The destructive two take `destroy_workspaces`, like `DELETE
//! /v1/workspaces/{id}` and its restore: whoever decides what the Trash
//! contains decides both ways. The reads take `manage_workspaces`, like the
//! listings they stand beside.
//!
//! **Rooms are fenced before the commit**, as the workspace delete does —
//! every target's manifest and document rooms are blocked and disconnected so
//! no writer keeps editing a durably trashed Workspace (§6.3 "fences
//! writers first") — and unblocked again if the Store refuses. The batch is a
//! confirmed operation over a captured list, so an occupied room is
//! disconnected rather than refused. Terminal sessions in the targets are not
//! closed here: the Gateway's lifecycle operation owns that for a single
//! Workspace, and a Profile-level operation of its own is not part of this
//! plan.

use super::*;
use koru_store_wire::{
    DeleteProfile, DeletedProfileListing, ProfileDeletionBatch, ProfileDeletionPreview,
    ProfileRestoreOutcome, RestoreProfile, UnassignedWorkspace,
};

pub async fn preview(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<ProfileId>,
) -> ApiResult<Json<ProfileDeletionPreview>> {
    caller.require(Scope::ManageWorkspaces)?;
    Ok(Json(lock(&state)?.profile_deletion_preview(id)?))
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<ProfileId>,
    Json(body): Json<DeleteProfile>,
) -> ApiResult<Json<ProfileDeletionBatch>> {
    caller.require(Scope::DestroyWorkspaces)?;
    let mut store = lock(&state)?;
    // The fence covers what the confirmation named. A target that has since
    // left the catalog lists no rooms and is refused as a stale preview by
    // the Store a moment later.
    let rooms = rooms_of(&mut store, &body.preview.targets);
    state.rooms.block_and_disconnect(&rooms);
    match store.delete_profile(id, &body.preview, now()) {
        Ok(batch) => {
            drop(store);
            state.notify_replication();
            state.notify_catalog_change();
            state.publish_catalog_event(koru_events::ProfileDeleted {
                profile_id: id.as_uuid(),
            });
            for (workspace, _) in &batch.targets {
                state.publish_catalog_event(koru_events::WorkspaceCatalogDeleted {
                    workspace_id: workspace.as_uuid(),
                });
            }
            Ok(Json(batch))
        }
        Err(error) => {
            state.rooms.unblock(&rooms);
            Err(error.into())
        }
    }
}

pub async fn deleted(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<Vec<DeletedProfileListing>>> {
    caller.require(Scope::ManageWorkspaces)?;
    let store = lock(&state)?;
    let mut listings = Vec::new();
    for deletion in store.profile_deletions(now())? {
        // The row is retained while a deletion is restorable; its name is
        // what a Trash surface says, read through the one accessor that sees
        // a hidden row.
        let name = store
            .profile_including_removed(deletion.profile)?
            .map(|profile| profile.name);
        listings.push(DeletedProfileListing { name, deletion });
    }
    Ok(Json(listings))
}

pub async fn restore(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<ProfileId>,
    Json(body): Json<RestoreProfile>,
) -> ApiResult<Json<ProfileRestoreOutcome>> {
    caller.require(Scope::DestroyWorkspaces)?;
    let mut store = lock(&state)?;
    let outcome = store.restore_profile(id, body.deletion)?;
    let rooms = rooms_of(&mut store, &outcome.restored);
    state.rooms.unblock(&rooms);
    drop(store);
    state.notify_replication();
    state.notify_catalog_change();
    state.publish_catalog_event(koru_events::ProfileRestored {
        profile_id: id.as_uuid(),
    });
    // Only what left the Trash is a catalog fact; what stayed, moved or was
    // purged is in the response (the registry row says so).
    for workspace in &outcome.restored {
        state.publish_catalog_event(koru_events::WorkspaceCatalogRestored {
            workspace_id: workspace.as_uuid(),
        });
    }
    Ok(Json(outcome))
}

pub async fn unassigned(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<Vec<UnassignedWorkspace>>> {
    caller.require(Scope::ManageWorkspaces)?;
    Ok(Json(lock(&state)?.unassigned_workspaces()?))
}

/// Every room of `workspaces`: each manifest and each document the Store
/// can still list. A Workspace it cannot list contributes none.
fn rooms_of(
    store: &mut crate::state::KoruStore,
    workspaces: &[WorkspaceId],
) -> Vec<crate::rooms::Room> {
    let mut rooms = Vec::with_capacity(workspaces.len());
    for workspace in workspaces {
        rooms.push(crate::rooms::Room::Manifest(*workspace));
        if let Ok(documents) = store.documents(*workspace) {
            rooms.extend(
                documents
                    .into_iter()
                    .map(|document| crate::rooms::Room::Document(document.id)),
            );
        }
    }
    rooms
}

#[cfg(test)]
mod tests;
