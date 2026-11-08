// SPDX-License-Identifier: MPL-2.0

//! The tasks query — `POST /v1/tasks/query`.
//!
//! **A second question asked of the one read model**, and the handler is
//! deliberately the attribute route's shape: check the scope, hand the typed
//! body to a port the daemon installed, return what it answers. The port exists
//! because the answer lives in `projection.db` and this HTTP crate links
//! neither `koru-projection` nor the node model — wire types cross the seam,
//! projection types never do ([ADR-287]).
//!
//! ## `Scope::ReadTasks` is not consulted, and that is the decision
//!
//! It stays a shipped value, intact, unenforced and unrenamed. Reading a task
//! *is* reading a node; the boundary here is one trusted user with peered
//! devices, and enforcing a second scope on this one route would be inventing
//! half of a permission model whose whole is deferred. So this asks for
//! [`Scope::ReadDocuments`], exactly as `/v1/attributes/query` and `/v1/search`
//! do, and a credential that can read documents can read the tasks in them.
//!
//! ## Why the call goes through `spawn_blocking`
//!
//! The port does real work on a real thread — a sweep request per workspace in
//! scope, then SQLite over the read model. Running that on a runtime worker
//! would park a thread that is supposed to be driving other requests, which is
//! the reason `routes::search` gives and it has not changed.
//!
//! [ADR-287]: ../../../docs/decisions/ADR-276-300.md#adr-287-taskread--the-tasks-query-is-a-second-question-asked-of-the-one-read-model

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, State};
use koru_store_core::errors::Code;
use koru_store_core::model::Scope;
use koru_store_wire::tasks::{MAX_LIMIT, TaskQuery, TaskQueryResponse};

use crate::auth::Caller;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Why the port did not answer. Split so a caller's mistake comes back as a
/// 400 with a reason and a broken read model as a 500 — the attribute route's
/// lesson, where one collapsed string made every typo in a priority token look
/// like storage trouble.
#[derive(Debug)]
pub enum TaskServiceError {
    /// The query itself is malformed — an unknown priority token, a due bound
    /// naming neither window, a tag that is not a `TagId`.
    Invalid(String),
    /// The query was fine and the read model was not.
    Failed(String),
}

/// What the API needs to answer a tasks query.
///
/// The daemon implements it over `projection.db` — sweep-first, the
/// `AttributePort` pattern: request a sweep for every workspace in scope, read
/// immediately, and name what the answer may be behind on in
/// [`TaskQueryResponse::updating_workspaces`] rather than waiting for it.
///
/// Blocking on purpose, and `Send + Sync` because one instance answers every
/// request.
pub trait TaskService: Send + Sync + 'static {
    fn query(&self, query: &TaskQuery) -> Result<TaskQueryResponse, TaskServiceError>;
}

/// The tasks matching a query, from the read model, without opening a document.
///
/// **A missing service is not an error**, the outline rule: a store built
/// without a projection thread still serves documents, and a caller gets the
/// honest empty answer — no rows, `projected: false`, and the workspaces it
/// asked about named as updating — rather than a failure. A Tasks view drawing
/// "still indexing" is right; one drawing a red banner because this store has
/// no projector is not.
pub async fn query(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Json(body): Json<TaskQuery>,
) -> ApiResult<Json<TaskQueryResponse>> {
    caller.require(Scope::ReadDocuments)?;
    match tokio::task::spawn_blocking(move || answer(&state, body)).await {
        Ok(Ok(response)) => Ok(Json(response)),
        Ok(Err(TaskServiceError::Invalid(why))) => Err(ApiError::new(Code::InvalidRequest, why)),
        Ok(Err(TaskServiceError::Failed(why))) => Err(ApiError::new(Code::StorageFailed, why)),
        Err(join) => Err(ApiError::new(
            Code::Internal,
            format!("the tasks thread did not finish: {join}"),
        )),
    }
}

/// The tasks query's answer, from whatever task service `state` holds.
///
/// **The one answer**, for the route above and for an in-process caller — the
/// Store worker's typed queue, which the shared tool layer reaches through
/// `EmbeddedStore::query_tasks` — so an embedded host gets the same page, the
/// same clamp and the same honest empty answer as HTTP, never a second
/// interpretation of a task. Blocking: the service sweeps and reads SQLite.
///
/// **Clamped, not refused.** A page is built in memory before it is written,
/// so the ceiling protects the store; a caller asking for ten thousand rows
/// is being hopeful rather than wrong.
pub fn answer(
    state: &AppState,
    mut query: TaskQuery,
) -> Result<TaskQueryResponse, TaskServiceError> {
    query.limit = query.limit.clamp(1, MAX_LIMIT);
    let Some(service) = state.task_service() else {
        return Ok(TaskQueryResponse {
            rows: Vec::new(),
            definitions: Vec::new(),
            tags: Vec::new(),
            groups: Vec::new(),
            projected: false,
            updating_workspaces: query.workspaces.clone(),
            limit: 0,
            next_after: None,
            restarted: false,
            covered: None,
        });
    };
    service.query(&query)
}

impl TaskServiceError {
    /// The Store error an in-process caller reports: a malformed query is the
    /// caller's mistake, a broken read model is storage — the route's split.
    pub fn into_error(self) -> koru_store_core::errors::Error {
        match self {
            Self::Invalid(why) => koru_store_core::errors::Error::new(Code::InvalidRequest, why),
            Self::Failed(why) => koru_store_core::errors::Error::new(Code::StorageFailed, why),
        }
    }
}

#[cfg(test)]
mod tests;
