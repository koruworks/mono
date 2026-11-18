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
//!
//! ## Two requests (OCR Plan 3.2; TECHNICAL §9, §10)
//!
//! `POST …/read-all` (*Read all pages*) and `POST …/read-again` (*Read
//! again*) record a machine-local request and wake the coordinator; the
//! reading itself is the worker's, later. **Refused, with a stable code,
//! where it could not be met here**: `blob_not_here` when the bytes are not on
//! this computer, `reader_unavailable` when finishing it needs recognition and
//! the reader is not installed, `invalid_request` when there is nothing to
//! ask for (no reading yet, not stopped at a limit, a request already
//! waiting). `GET` answers the same question beforehand in
//! [`ReadingAnswer::offers`]. Both are `edit_documents`: what they set going
//! ends in a write to a synced document.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use koru_store_core::errors::{Code, Error};
use koru_store_core::ids::WorkspaceId;
use koru_store_core::model::{BlobHash, Scope};
use koru_store_wire::reader::ReaderItems;
use koru_store_wire::readings::{ReadRequestKind, ReadRequested, ReadingAnswer};

use crate::auth::Caller;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// **The daemon's answer to "what was read out of these bytes here?"**
pub trait ReadingService: Send + Sync + 'static {
    fn correct(
        &self,
        _workspace: WorkspaceId,
        _hash: &BlobHash,
        _request: koru_store_wire::readings::CorrectReading,
    ) -> Result<koru_store_wire::readings::ReadingCorrections, Error> {
        Err(Error::new(
            Code::ProjectionUnavailable,
            "reading corrections are unavailable",
        ))
    }
    /// The winning reading and local state of a live blob, including unread
    /// items. `None` means no known live item. `Err` is a mirror that could not be
    /// read, with SQLite's message, which never carries a row's value.
    fn reading(
        &self,
        workspace: WorkspaceId,
        hash: &BlobHash,
    ) -> Result<Option<ReadingAnswer>, String>;

    /// **Record a person's request** to read `hash` in `workspace` in full
    /// or again, and wake whoever meets it. Refused as the module note says.
    /// A store that runs no reader refuses every request.
    fn request(
        &self,
        workspace: WorkspaceId,
        hash: &BlobHash,
        kind: ReadRequestKind,
    ) -> Result<ReadRequested, Error> {
        let _ = (workspace, hash, kind);
        Err(Error::new(
            Code::ReaderUnavailable,
            "this store reads nothing itself",
        ))
    }

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
/// Live unread items return local facts with no status or pages. `null` means
/// no known item; an unknown workspace is a 404 from the catalog.
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
    let answer = tokio::task::spawn_blocking(move || service.reading(workspace, &hash))
        .await
        .map_err(|_| ApiError::bare(Code::Internal))?
        .map_err(|error| ApiError::new(Code::StorageFailed, error))?;
    Ok(Json(answer))
}

pub async fn correct(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, hash)): Path<(String, String)>,
    Json(body): Json<koru_store_wire::readings::CorrectReading>,
) -> ApiResult<Json<koru_store_wire::readings::ReadingCorrections>> {
    caller.require(Scope::EditDocuments)?;
    let workspace =
        WorkspaceId::parse(&workspace).ok_or_else(|| ApiError::bare(Code::InvalidId))?;
    let hash = BlobHash::parse(&hash).map_err(|_| ApiError::bare(Code::InvalidRequest))?;
    let service = state
        .reading_service()
        .ok_or_else(|| ApiError::bare(Code::ProjectionUnavailable))?;
    let answer = tokio::task::spawn_blocking(move || service.correct(workspace, &hash, body))
        .await
        .map_err(|_| ApiError::bare(Code::Internal))??;
    Ok(Json(answer))
}

/// `POST /v1/readings/{workspace}/{hash}/read-all` — *Read all pages*.
pub async fn read_all(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, hash)): Path<(String, String)>,
) -> ApiResult<Json<ReadRequested>> {
    request(state, caller, workspace, hash, ReadRequestKind::ReadAll)
}

/// `POST /v1/readings/{workspace}/{hash}/read-again` — *Read again*.
pub async fn read_again(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, hash)): Path<(String, String)>,
) -> ApiResult<Json<ReadRequested>> {
    request(state, caller, workspace, hash, ReadRequestKind::ReadAgain)
}

/// Render and recognize each page even when embedded text is present.
pub async fn force_ocr(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, hash)): Path<(String, String)>,
) -> ApiResult<Json<ReadRequested>> {
    request(state, caller, workspace, hash, ReadRequestKind::ForceOcr)
}

fn request(
    state: Arc<AppState>,
    caller: Caller,
    workspace: String,
    hash: String,
    kind: ReadRequestKind,
) -> ApiResult<Json<ReadRequested>> {
    caller.require(Scope::EditDocuments)?;
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
            Code::ReaderUnavailable,
            "this store is not running a readings mirror",
        )
    })?;
    Ok(Json(service.request(workspace, &hash, kind)?))
}
