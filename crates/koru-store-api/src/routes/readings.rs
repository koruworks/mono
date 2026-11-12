// SPDX-License-Identifier: MPL-2.0

//! `/v1/readings/*` — the words read out of files and pictures (OCR TECHNICAL
//! §10).
//!
//! **Answered by a port the daemon implements**, for the reason search is: the
//! readings mirror and the reading document's shape live above this crate,
//! which links neither. A store whose mirror would not open answers
//! `projection_unavailable` — a derived read model that is not here, not a
//! store that has lost anything.
//!
//! This is the one route that returns a file's words, so it takes one item at
//! a time, named by its bytes, from someone who can read the workspace.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use koru_store_core::errors::Code;
use koru_store_core::ids::WorkspaceId;
use koru_store_core::model::{BlobHash, Scope};
use koru_store_wire::reader::ReaderItems;
use koru_store_wire::readings::ReadingAnswer;

use crate::auth::Caller;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// **The daemon's answer to "what was read out of these bytes here?"**
pub trait ReadingService: Send + Sync + 'static {
    /// The winning reading of `hash` in `workspace`, or `None` when nothing
    /// has been read of it there yet. `Err` is a mirror that could not be
    /// read, with SQLite's message, which never carries a row's value.
    fn reading(
        &self,
        workspace: WorkspaceId,
        hash: &BlobHash,
    ) -> Result<Option<ReadingAnswer>, String>;

    /// **The items behind the Settings row's *could not be read* count**
    /// (OCR Plan 2.5), every workspace on this computer: ids and reason
    /// tokens, never a name. Reads the derived read models on connections of
    /// its own and takes no Store lock. A daemon that answers nothing here
    /// says so with an empty list.
    fn items(&self) -> Result<ReaderItems, String> {
        Ok(ReaderItems::default())
    }
}

/// `GET /v1/readings/reader/items` — what could not be read on this computer,
/// item by item.
///
/// **Through `spawn_blocking`**, because the answer is a walk of the
/// projection's readable references and the mirror's outcomes — reads of
/// SQLite that must not sit on the runtime a keystroke's search shares.
/// `projection_unavailable` where no readings mirror runs, the same answer
/// one reading gives.
pub async fn items(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<ReaderItems>> {
    caller.require(Scope::ReadDocuments)?;
    let service = state.reading_service().ok_or_else(|| {
        ApiError::new(
            Code::ProjectionUnavailable,
            "this store is not running a readings mirror",
        )
    })?;
    let answer = tokio::task::spawn_blocking(move || service.items())
        .await
        .map_err(|_| ApiError::new(Code::Internal, "the items read did not finish"))?
        .map_err(|error| ApiError::new(Code::StorageFailed, error))?;
    Ok(Json(answer))
}

/// `GET /v1/readings/{workspace}/{hash}` — the winning reading, or `null`.
///
/// `null` is "nothing read here yet", which is an answer; an unknown
/// workspace is a 404 from the catalog, never a `null` that would read as
/// one.
pub async fn reading(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, hash)): Path<(String, String)>,
) -> ApiResult<Json<Option<ReadingAnswer>>> {
    caller.require(Scope::ReadDocuments)?;
    let workspace = WorkspaceId::parse(&workspace)
        .ok_or_else(|| ApiError::new(Code::InvalidId, "not a uuid"))?;
    let hash = BlobHash::parse(&hash)
        .map_err(|_| ApiError::new(Code::InvalidRequest, "not a blob address"))?;
    state
        .store
        .lock()
        .map_err(|_| {
            ApiError::new(
                Code::Internal,
                "the store lock was poisoned by an earlier failure",
            )
        })?
        .workspace(workspace)?;
    let service = state.reading_service().ok_or_else(|| {
        ApiError::new(
            Code::ProjectionUnavailable,
            "this store is not running a readings mirror",
        )
    })?;
    let answer = service
        .reading(workspace, &hash)
        .map_err(|error| ApiError::new(Code::StorageFailed, error))?;
    Ok(Json(answer))
}
