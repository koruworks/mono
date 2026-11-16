// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::{SetTransportMode, TransportMode, TransportSubject};
use koru_store_wire::{
    ProfileTransportSettings, SetWorkspaceTransportMode, WorkspaceTransportSettings,
};

fn settings(
    store: &crate::state::KoruStore,
    id: ProfileId,
) -> koru_store_core::errors::Result<ProfileTransportSettings> {
    let group = store.transport_mode_group(id)?;
    let workspaces = store.workspaces()?;
    Ok(ProfileTransportSettings {
        mode: group.profile.mode.as_storage().into(),
        explicit_choice: group.profile.explicit_choice,
        revision: group.profile.revision,
        workspaces: group
            .children
            .iter()
            .filter_map(|row| {
                let id = row.subject.workspace()?;
                let workspace = workspaces.iter().find(|workspace| workspace.id == id)?;
                Some(WorkspaceTransportSettings {
                    id: id.to_storage(),
                    name: workspace.name.clone(),
                    mode: row.mode.as_storage().into(),
                    explicit_choice: row.explicit_choice,
                    revision: row.revision,
                })
            })
            .collect(),
    })
}

pub async fn get_profile_transport_mode(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<Json<ProfileTransportSettings>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    let store = lock(&state)?;
    Ok(Json(settings(&store, id)?))
}

pub async fn initialize_profile_transport_mode(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<SetProfileTransportMode>,
) -> ApiResult<Json<ProfileTransportSettings>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    let mode = parse_mode(&body.mode)?;
    let (result, changed) = {
        let mut store = lock(&state)?;
        let changed = store
            .initialize_profile_transport_mode(id, mode)?
            .iter()
            .any(|outcome| outcome.wrote());
        (settings(&store, id)?, changed)
    };
    if changed {
        state.notify_catalog_change();
        state.notify_replication();
    }
    Ok(Json(result))
}

pub async fn set_workspace_transport_mode(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<SetWorkspaceTransportMode>,
) -> ApiResult<Json<ProfileTransportSettings>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = WorkspaceId::parse(&id).ok_or_else(invalid_id)?;
    let mode = parse_mode(&body.mode)?;
    let result = {
        let mut store = lock(&state)?;
        let subject = TransportSubject::Workspace(id);
        let owner = store.transport_mode(subject)?.owner;
        let outcome = store.set_transport_mode(SetTransportMode {
            subject,
            mode,
            expected_revision: body.expected_revision,
        })?;
        if matches!(outcome, koru_store_core::TransportModeCommit::Superseded(_)) {
            return Err(ApiError::new(
                Code::InvalidRequest,
                "sync settings changed; read them again",
            ));
        }
        settings(&store, owner)?
    };
    state.notify_catalog_change();
    state.notify_replication();
    if mode.allows_cloud()
        && let Some(blobs) = state.blob_transfer_status_service()
    {
        blobs.enabled(id);
    }
    Ok(Json(result))
}

fn parse_mode(value: &str) -> ApiResult<TransportMode> {
    TransportMode::parse(value).map_err(|_| {
        ApiError::new(
            Code::InvalidRequest,
            "mode is not one of local_only, p2p, cloud",
        )
    })
}

#[cfg(test)]
mod tests;
