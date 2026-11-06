// SPDX-License-Identifier: MPL-2.0

//! The Store worker's typed queue, served.
//!
//! One loop over [`WorkerCommand`]s against one [`AppState`]: Store jobs run
//! under its lock in arrival order, search and the tasks query ask the ports
//! the composition installed, and an HTTP request goes through the trusted
//! router. The daemon serves it beside its listener; an in-memory composition
//! ([`crate::memory`]) serves the same loop on its own thread, so a
//! [`StoreWorkerClient`](super::StoreWorkerClient) means the same thing over
//! either.

use std::sync::Arc;

use koru_store_api::AppState;
use koru_store_core::errors::{Code, Error};

use super::{StoreWorkerInfo, WorkerCommand, WorkerError};

pub(crate) async fn serve(
    mut requests: tokio::sync::mpsc::Receiver<WorkerCommand>,
    state: Arc<AppState>,
) {
    use tower::ServiceExt as _;
    let trusted_router = koru_store_api::router_for_trusted_requests(Arc::clone(&state));
    while let Some(command) = requests.recv().await {
        match command {
            WorkerCommand::Noop(reply) => {
                let _ = reply.send(());
            }
            WorkerCommand::Status(reply) => {
                let _ = reply.send(StoreWorkerInfo {
                    api_version: state.api_version(),
                    capabilities: state
                        .capabilities()
                        .iter()
                        .map(|capability| (*capability).to_owned())
                        .collect(),
                    channel: state.channel.clone(),
                    started_at: state.started_at,
                });
            }
            WorkerCommand::Http {
                mut request,
                caller,
                reply,
            } => {
                request.extensions_mut().insert(caller);
                let router = trusted_router.clone();
                tokio::spawn(async move {
                    let response = router
                        .oneshot(request)
                        .await
                        .expect("infallible Store router");
                    let _ = reply.send(response);
                });
            }
            WorkerCommand::Store(task) => {
                static POISON_REPORTED: std::sync::Once = std::sync::Once::new();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match state
                    .store
                    .lock()
                {
                    Ok(mut store) => task(Ok(&mut store)),
                    Err(_) => {
                        POISON_REPORTED.call_once(|| {
                            tracing::error!("Store worker state is poisoned");
                        });
                        task(Err(WorkerError::Poisoned));
                    }
                }));
                if result.is_err() {
                    POISON_REPORTED.call_once(|| {
                        tracing::error!("Store worker task panicked and poisoned Store state");
                    });
                }
            }
            WorkerCommand::WatchCloud { dataset, reply } => {
                let _ = reply.send(state.cloud_changes.subscribe(dataset));
            }
            WorkerCommand::Search { request, reply } => {
                let _ = reply.send(search(&state, &request));
            }
            WorkerCommand::Tasks { query, reply } => {
                // The task port sweeps and reads SQLite: real work on a real
                // thread, for the reason the HTTP route gives, and never on
                // this dispatcher, which serves every other command.
                let state = Arc::clone(&state);
                tokio::task::spawn_blocking(move || {
                    let answer = koru_store_api::routes::tasks::answer(&state, *query)
                        .map_err(koru_store_api::routes::tasks::TaskServiceError::into_error);
                    let _ = reply.send(answer);
                });
            }
            WorkerCommand::CreateDocument {
                workspace,
                title,
                kind,
                attribution,
                at,
                reply,
            } => {
                let result = match state.store.lock() {
                    Ok(mut store) => store.create_document_attributed(
                        workspace,
                        &title,
                        &kind,
                        None,
                        None,
                        attribution.as_deref(),
                        at,
                    ),
                    Err(_) => Err(Error::new(Code::Internal, "Store worker state is poisoned")),
                };
                let result = result.map(|(document, delta)| {
                    state.broadcast_manifest(workspace, delta);
                    state.notify_replication();
                    document
                });
                let _ = reply.send(result);
            }
            WorkerCommand::ApplyDocumentUpdate {
                workspace,
                document,
                update,
                at,
                reply,
            } => {
                let result = match state.store.lock() {
                    Ok(mut store) => store
                        .apply_update(
                            workspace,
                            document,
                            update.clone(),
                            at,
                            koru_store_core::ports::UpdateOrigin::Store,
                        )
                        .and_then(|()| store.flush(workspace, document, at)),
                    Err(_) => Err(Error::new(Code::Internal, "Store worker state is poisoned")),
                };
                let result = result.map(|committed| {
                    if committed.is_some() {
                        state.broadcast_document(document, update);
                        state.notify_replication();
                    }
                });
                let _ = reply.send(result);
            }
            WorkerCommand::SeedDocument {
                workspace,
                title,
                kind,
                parent,
                index,
                scope,
                idempotency_key,
                request_hash,
                initial_update,
                attribution,
                at,
                reply,
            } => {
                let result = match state.store.lock() {
                    Ok(mut store) => store.create_seeded_document_attributed(
                        workspace,
                        &title,
                        &kind,
                        parent,
                        index,
                        &scope,
                        &idempotency_key,
                        &request_hash,
                        None,
                        initial_update,
                        attribution.as_deref(),
                        at,
                    ),
                    Err(_) => Err(Error::new(Code::Internal, "Store worker state is poisoned")),
                };
                let result = result.map(|creation| {
                    if let Some(delta) = creation.manifest_delta.as_ref() {
                        let delta = delta.clone();
                        state.broadcast_manifest(workspace, delta);
                        state.notify_replication();
                    }
                    creation
                });
                let _ = reply.send(result);
            }
        }
    }
}

/// One search, from the index the composition installed.
///
/// Embedded callers arrive with their explicit grant set already written into
/// the query. That set is kept as an authorization scope too, so an index can
/// never widen an AI search if query filtering changes later.
pub(crate) fn search(
    state: &AppState,
    request: &koru_store_wire::search::SearchRequest,
) -> Result<koru_store_wire::search::SearchResponse, Error> {
    let scope = if request.query.workspaces.is_empty() {
        koru_store_api::routes::search::WorkspaceSearchScope::AllLocal
    } else {
        koru_store_api::routes::search::WorkspaceSearchScope::Allowed(
            request.query.workspaces.iter().copied().collect(),
        )
    };
    state
        .search_service()
        .ok_or_else(|| Error::new(Code::SearchUnavailable, "search is unavailable"))
        .and_then(|search| search.search(&scope, request))
}
