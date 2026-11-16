// SPDX-License-Identifier: MPL-2.0

//! `/v1/debug/*` and `/v1/metrics` — what a person needs to diagnose the store.
//!
//! ## Metadata only. Never content.
//!
//! Every field here is a count, a size, a version, an instant or an id. **No
//! document text, no title, no filename, no blob bytes.** That is not a
//! politeness — these are the routes most likely to end up pasted into a bug
//! report, a support thread or a log aggregator, and the one thing that must
//! never travel that way is what somebody wrote.
//!
//! The one string that is not a number is the data directory, and it appears
//! **once**, opaque, on `/v1/debug/store`. A client must not take it apart:
//! knowing where a database is is not the same as being allowed to open it, and
//! making the physical layout visible makes it a compatibility surface.
//!
//! ## Why these exist rather than a log file
//!
//! A log says what happened while somebody was watching. These say what is true
//! now — which version vector a document is on, how many updates are waiting
//! behind the last checkpoint, whether anything was recovered at start-up.
//! Those are the questions asked *after* something went wrong.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use koru_store_core::errors::Code;
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_core::model::Scope;
// **The reports are `koru_store_wire::internal`**, not declarations of their
// own: typed so our server and our client break together when a field is
// renamed, and in `internal` because they carry no promise to anybody else.
use koru_store_wire::internal::{
    DocumentReport, Metrics, RoomReport, StoreReport, WorkspaceReport,
};

use crate::auth::Caller;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// What the projection layer is doing.
///
/// Whatever the daemon last published, verbatim — pending, projected, failed,
/// whether a sweep is running and how much of it is left. `null` on a build
/// that runs no projector, which is a real answer rather than an error.
pub async fn projection(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<serde_json::Value>> {
    caller.require(Scope::ReadDocuments)?;
    let value = state
        .projection
        .lock()
        .map(|v| v.clone())
        .unwrap_or(serde_json::Value::Null);
    Ok(Json(value))
}

/// What the search index is doing.
///
/// Whatever the daemon last published, verbatim — queue depth, indexed,
/// removed, failed, and how much it holds. `null` on a build that runs no
/// index. **No titles, no query text, no fragment text**: this is the route
/// most likely to be pasted into a bug report.
pub async fn search(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<serde_json::Value>> {
    caller.require(Scope::ReadDocuments)?;
    let value = state
        .search
        .lock()
        .map(|v| v.clone())
        .unwrap_or(serde_json::Value::Null);
    Ok(Json(value))
}

/// What reading words out of files and pictures is doing.
///
/// Whatever the daemon last published, verbatim — what is queued and why,
/// the job running now, what has been read and what failed by code, whether
/// the reading worker is up and why not, how far the readings mirror is
/// behind (OCR TECHNICAL §14). `null` on a build that runs no readings mirror.
/// **Ids, counts and stages only**: no file name, no title, no word.
pub async fn readings(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<serde_json::Value>> {
    caller.require(Scope::ReadDocuments)?;
    let value = state
        .readings
        .lock()
        .map(|v| v.clone())
        .unwrap_or(serde_json::Value::Null);
    Ok(Json(value))
}

/// What the attribute query service is doing.
///
/// Counters only — queries served, rows answered, query milliseconds. Never a
/// key, a value, a tag name or an excerpt: this is the route most tempted to
/// answer with what somebody filtered on. `null` on a build that runs no
/// projection.
pub async fn attributes(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<serde_json::Value>> {
    caller.require(Scope::ReadDocuments)?;
    let value = state
        .attributes
        .lock()
        .map(|v| v.clone())
        .unwrap_or(serde_json::Value::Null);
    Ok(Json(value))
}

/// What the tasks query service is doing.
///
/// Counters only — queries served, rows answered, sweeps requested, cursor
/// restarts, query milliseconds. **Never a title, a breadcrumb, a due date or a
/// task reference**: a reference names a document and a node, which is the one
/// thing a route this likely to be pasted into a bug report must not carry.
/// `null` on a build that runs no projection.
pub async fn tasks(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<serde_json::Value>> {
    caller.require(Scope::ReadDocuments)?;
    let value = state
        .tasks
        .lock()
        .map(|v| v.clone())
        .unwrap_or(serde_json::Value::Null);
    Ok(Json(value))
}

/// Volatile replication readiness and counts. Never peer or workspace names.
pub async fn sync(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<serde_json::Value>> {
    caller.require(Scope::ReadDocuments)?;
    let value = state
        .sync_control
        .lock()
        .map(|control| serde_json::to_value(control.status()).unwrap_or(serde_json::Value::Null))
        .unwrap_or(serde_json::Value::Null);
    Ok(Json(value))
}

// ------------------------------------------------------------------ the store

pub async fn store(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<StoreReport>> {
    caller.require(Scope::ReadDocuments)?;
    let store = lock(&state)?;
    let known = store.workspaces()?.len();
    Ok(Json(StoreReport {
        channel: state.channel.clone(),
        pid: std::process::id(),
        data_directory: state.root.clone(),
        started_at: state.started_at,
        uptime_ms: koru_store_core::model::Timestamp::now().millis() - state.started_at,
        api_version: state.api_version(),
        catalog_schema_version: koru_store_core::version::CATALOG_SCHEMA_VERSION,
        workspace_schema_version: koru_store_core::version::WORKSPACE_SCHEMA_VERSION,
        crdt_schema_version: koru_store_core::version::CRDT_SCHEMA_VERSION,
        bundle_format_version: koru_store_core::version::BUNDLE_FORMAT_VERSION,
        // As a string: a u64 loses precision in JSON's number type, and a peer
        // id that silently rounds is worse than one that is awkward to read.
        peer_id: store.peer_id().to_string(),
        workspaces_known: known,
        workspaces_open: store.open_workspaces(),
        documents_open: store.open_documents(),
        rooms: state
            .rooms
            .occupied()
            .into_iter()
            .map(|(room, subscribers)| RoomReport { room, subscribers })
            .collect(),
        recoveries: store.recoveries().to_vec(),
    }))
}

// -------------------------------------------------------------- a workspace

pub async fn workspace(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<Json<WorkspaceReport>> {
    caller.require(Scope::ReadDocuments)?;
    let id = WorkspaceId::parse(&id).ok_or_else(invalid_id)?;
    let mut store = lock(&state)?;
    let blobs = store.blobs(id)?;
    let record = store.workspace_including_deleted(id)?;
    let manifest_document = store.replica_manifest_id_including_deleted(id)?;
    let manifest_deletion = match store.workspace_manifest_deletion(id)? {
        Some(Some(_)) => "active",
        Some(None) => "restored",
        None => "unset",
    };
    Ok(Json(WorkspaceReport {
        workspace: id,
        database_bytes: store.database_bytes(id)?,
        documents: store.documents(id)?.len(),
        blobs: blobs.len(),
        blob_bytes: blobs.iter().map(|b| b.size).sum(),
        lineage: store.lineage(id)?,
        manifest_document,
        deleted_at_millis: record.deleted_at.map(|at| at.millis()),
        manifest_deletion: Some(manifest_deletion.to_owned()),
        icon: record.icon.clone(),
        color: record.color.clone(),
        // Asked after the Store's own numbers and answered from memory: the
        // engine's counters are a handful of integers and are always live,
        // so there is no flag anybody has to have switched on before the
        // incident.
        blob_transfer: state
            .blob_transfer_status_service()
            .map(|service| service.status(id)),
    }))
}

// --------------------------------------------------------------- a document

pub async fn document(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<Json<DocumentReport>> {
    caller.require(Scope::ReadDocuments)?;
    let id = DocumentId::parse(&id).ok_or_else(invalid_id)?;
    let mut store = lock(&state)?;
    let workspace = store
        .locate(id)?
        .ok_or_else(|| ApiError::bare(Code::DocumentNotFound))?;
    let report = store.document_report(workspace, id)?;

    Ok(Json(DocumentReport {
        document: id,
        workspace,
        kind: report.kind.as_str().to_string(),
        version_vector: hex(&report.version_vector),
        frontiers: hex(&report.frontiers),
        latest_checkpoint: report.latest_checkpoint,
        checkpoints: report.checkpoints,
        pending_updates: report.pending_updates,
        schema_version: report.schema_version,
        trashed_at_millis: report.trashed_at.map(|at| at.millis()),
        subscribers: state.rooms.subscribers(&crate::rooms::Room::Document(id)),
        updated_at: report.updated_at.millis(),
    }))
}

// ----------------------------------------------------------------- metrics

pub async fn metrics(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<Metrics>> {
    caller.require(Scope::ReadDocuments)?;
    let store = lock(&state)?;
    let rooms = state.rooms.occupied();
    Ok(Json(Metrics {
        uptime_ms: koru_store_core::model::Timestamp::now().millis() - state.started_at,
        workspaces_known: store.workspaces()?.len(),
        workspaces_open: store.open_workspaces(),
        documents_open: store.open_documents(),
        rooms_occupied: rooms.len(),
        subscribers: rooms.iter().map(|(_, n)| n).sum(),
        recoveries: store.recoveries().len(),
    }))
}

// ------------------------------------------------------------------ shared

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn invalid_id() -> ApiError {
    ApiError::new(Code::InvalidId, "not a uuid")
}

fn lock(state: &Arc<AppState>) -> ApiResult<std::sync::MutexGuard<'_, crate::state::KoruStore>> {
    state.store.lock().map_err(|_| {
        ApiError::new(
            Code::Internal,
            "the store lock was poisoned by an earlier failure",
        )
    })
}
