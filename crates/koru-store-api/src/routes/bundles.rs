// SPDX-License-Identifier: MPL-2.0

//! `.koru` export and import over the shared binary bundle framing.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::response::IntoResponse;
use koru_store_core::bundle::{ImportMode, ImportPublication};
use koru_store_core::errors::{Code, Error};
use koru_store_core::ids::WorkspaceId;
use koru_store_core::model::{Scope, Timestamp, Workspace};
use koru_store_wire::ImportQuery;

use crate::auth::Caller;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

// Kept public for the route-level framing tests. The implementation lives in
// the wire crate so clients and the server cannot drift.
pub use koru_store_wire::bundle::{MAGIC, decode, encode};

pub async fn export(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(workspace): Path<String>,
) -> ApiResult<axum::response::Response> {
    caller.require(Scope::ReadDocuments)?;
    let workspace = WorkspaceId::parse(&workspace)
        .ok_or_else(|| ApiError::new(Code::InvalidId, "not a uuid"))?;
    let bundle = lock(&state)?.export_workspace(workspace, Timestamp::now())?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        encode(&bundle)?,
    )
        .into_response())
}

pub async fn import(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Query(query): Query<ImportQuery>,
    body: Result<axum::body::Bytes, BytesRejection>,
) -> ApiResult<Json<Workspace>> {
    caller.require(Scope::ManageWorkspaces)?;
    let mode = ImportMode::parse(&query.mode).ok_or_else(|| {
        ApiError::new(
            Code::InvalidRequest,
            "mode must be `restore` or `duplicate`; there is no default",
        )
    })?;
    // Capture extractor failures so every failed API request keeps Koru's
    // `{ code, message }` contract instead of leaking Axum's plain-text body
    // through the client as an `internal` error.
    let body = body.map_err(|error| {
        ApiError::new(
            Code::InvalidRequest,
            format!("could not read the import body: {error}"),
        )
    })?;
    let bundle = decode(&body)?;

    let workspace = {
        let mut store = lock(&state)?;
        let imported = store.import_workspace(&bundle, mode, query.owner, Timestamp::now())?;
        // The wire answers the Workspace and nothing else, so the one fact the
        // response cannot carry is said here: this Restore resumed the staging
        // an interrupted one left behind rather than starting over.
        if imported.publication == ImportPublication::Resumed {
            tracing::info!(workspace = %imported.workspace,
                "the import resumed the staged repository an interrupted Restore left behind");
        }
        store.workspace(imported.workspace)?
    };
    // After the commit, and named for what happened: a restored workspace keeps
    // the bundle's ids and is not a new one, which is why this is not
    // `workspace.catalog.created`.
    state.notify_catalog_change();
    state.publish_catalog_event(koru_events::WorkspaceCatalogImported {
        workspace_id: workspace.id.as_uuid(),
        mode: match mode {
            ImportMode::Restore => koru_events::WorkspaceImportMode::Restore,
            ImportMode::Duplicate => koru_events::WorkspaceImportMode::Duplicate,
        },
    });
    Ok(Json(workspace))
}

fn lock(state: &Arc<AppState>) -> ApiResult<std::sync::MutexGuard<'_, crate::state::KoruStore>> {
    state.store.lock().map_err(|_| {
        ApiError::from(Error::new(
            Code::Internal,
            "the store lock was poisoned by an earlier failure",
        ))
    })
}

#[cfg(test)]
mod tests;
