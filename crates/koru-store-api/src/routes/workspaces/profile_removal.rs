// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_wire::{BeginProfileRemoval, ProfileRemovalAttempt, RemovalMark};

pub async fn read(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<ProfileId>,
) -> ApiResult<Json<Option<RemovalMark>>> {
    caller.require(Scope::ReadDocuments)?;
    Ok(Json(lock(&state)?.profile_removal(id)?))
}

pub async fn begin(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<ProfileId>,
    Json(body): Json<BeginProfileRemoval>,
) -> ApiResult<Json<RemovalMark>> {
    caller.require(Scope::DestroyWorkspaces)?;
    let mark = lock(&state)?.begin_profile_removal(id, body.kind, now())?;
    state.notify_catalog_change();
    state.publish_catalog_event(koru_events::ProfileRemovalChanged {
        profile_id: id.as_uuid(),
    });
    Ok(Json(mark))
}

pub async fn stop(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<ProfileId>,
    Json(body): Json<ProfileRemovalAttempt>,
) -> ApiResult<Json<()>> {
    caller.require(Scope::DestroyWorkspaces)?;
    lock(&state)?.stop_profile_removal(id, body.attempt)?;
    state.notify_catalog_change();
    state.publish_catalog_event(koru_events::ProfileRemovalChanged {
        profile_id: id.as_uuid(),
    });
    Ok(Json(()))
}

pub async fn keep(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<ProfileId>,
    Json(body): Json<ProfileRemovalAttempt>,
) -> ApiResult<Json<()>> {
    caller.require(Scope::DestroyWorkspaces)?;
    lock(&state)?.keep_profile(id, body.attempt)?;
    state.notify_catalog_change();
    state.publish_catalog_event(koru_events::ProfileRemovalChanged {
        profile_id: id.as_uuid(),
    });
    Ok(Json(()))
}

#[cfg(test)]
mod tests;
