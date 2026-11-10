// SPDX-License-Identifier: MPL-2.0

//! JSON-RPC over one `POST`: the stateless Streamable-HTTP transport of the
//! run endpoint.
//!
//! Authority first, on every message including `initialize`: the bearer
//! is resolved through the artifact domain's [`RunAuthority`] port, and it
//! must name the run in the path — a token for one run at another run's
//! URL is refused as unknown. Then the body, which is one SDK
//! `ClientJsonRpcMessage`: a request is answered `200` with the JSON-RPC
//! response, a notification (`notifications/initialized`, a cancellation)
//! is `202` with no body, and anything else a client might send — a
//! response to a request this server never made — is `202` too. `GET` is
//! `405`: the server never speaks first, so there is no stream to open;
//! `DELETE` is `405`: there is no session to end.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use koru_artifact_api::Resolution;
use koru_artifact_core::{AuthorityRefusal, RunAuthorityRecord, RunId};
use koru_artifact_wire::run_tools::ToolFailure;
use rmcp::model::{
    ClientJsonRpcMessage, ClientRequest, ErrorCode, ErrorData, Implementation, InitializeResult,
    JsonRpcMessage, ListToolsResult, ProtocolVersion, RequestId, ServerCapabilities,
    ServerJsonRpcMessage, ServerResult,
};

use super::{native, tools};
use crate::GatewayState;
use koru_delegate_core::RunToolAuthority;

/// The most bytes one JSON-RPC message may carry. Arguments are a path, a
/// title, an id or a query; the bytes of a result never travel here.
const MESSAGE_LIMIT: usize = 64 * 1024;

fn refuse(status: StatusCode, failure: ToolFailure) -> Response {
    let mut response = Json(failure).into_response();
    *response.status_mut() = status;
    response
}

fn refused(refusal: AuthorityRefusal) -> Response {
    refuse(StatusCode::UNAUTHORIZED, refusal.into())
}

/// The bearer, resolved. Absent, malformed, unknown, stale, revoked, or
/// for another run: refused with the code that says which.
async fn authorize(
    state: &Arc<GatewayState>,
    run: RunId,
    headers: &HeaderMap,
) -> Result<Arc<RunAuthorityRecord>, Response> {
    let Some(domain) = state.artifact_domain() else {
        return Err(refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            ToolFailure::new(
                "artifacts_unavailable",
                "The artifact store is not available.",
            ),
        ));
    };
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .unwrap_or_default()
        .to_owned();
    let resolution = tokio::task::spawn_blocking(move || domain.resolve(&bearer))
        .await
        .map_err(|_| {
            refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                ToolFailure::new(
                    "artifacts_unavailable",
                    "The artifact store did not answer.",
                ),
            )
        })?
        .map_err(|failure| {
            refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                ToolFailure::new(failure.code, failure.message),
            )
        })?;
    match resolution {
        Resolution::Authorized(record) if record.run == run => Ok(Arc::new(record)),
        // A token for another run: not this endpoint's, and it learns
        // nothing about whether this run exists.
        Resolution::Authorized(_) => Err(refused(AuthorityRefusal::Unknown)),
        Resolution::Refused(refusal) => Err(refused(refusal)),
    }
}

fn jsonrpc_error(status: StatusCode, id: Option<RequestId>, error: ErrorData) -> Response {
    let message: ServerJsonRpcMessage = JsonRpcMessage::error(error, id);
    let mut response = Json(message).into_response();
    *response.status_mut() = status;
    response
}

fn answer(id: RequestId, result: ServerResult) -> Response {
    let message: ServerJsonRpcMessage = JsonRpcMessage::response(result, id);
    Json(message).into_response()
}

/// What `initialize` answers. The protocol version is the client's when
/// this SDK knows it, else the latest this SDK speaks — the SDK server's
/// own negotiation rule.
fn initialize(offered: &ProtocolVersion, bot: bool) -> InitializeResult {
    let negotiated = if ProtocolVersion::KNOWN_VERSIONS.contains(offered) {
        offered.clone()
    } else {
        ProtocolVersion::LATEST
    };
    InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
        .with_protocol_version(negotiated)
        .with_server_info(Implementation::new(
            "koru-run",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(if bot {
            "This Bot endpoint exposes four scoped native document tools. Create ordinary pages in the explicit workspace; use a stable operation_id for each mutation. Access refusals require an owner grant and explicit continuation. A native result is Saved only after durable verification and Run registration."
        } else {
            "This is the run's artifact endpoint. Write results under your .artifacts/<RunId>/ folder, then register them with `create`; use `create_version` for a further version of a result, `get`/`read` to inspect one, `search` to find earlier results in the workspaces granted to this run, and `attach_to_run` to record one as your input."
        })
}

pub(super) async fn handle(
    State(state): State<Arc<GatewayState>>,
    Path(run): Path<String>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(run) = run.parse::<RunId>() else {
        return refuse(
            StatusCode::NOT_FOUND,
            ToolFailure::new(
                "invalid_request",
                "The run endpoint's path is not a canonical v4 UUID.",
            ),
        );
    };
    if method != Method::POST {
        let mut response = refuse(
            StatusCode::METHOD_NOT_ALLOWED,
            ToolFailure::new(
                "method_not_allowed",
                "The run endpoint is stateless: POST one JSON-RPC message; there is no stream to GET and no session to DELETE.",
            ),
        );
        response
            .headers_mut()
            .insert(header::ALLOW, "POST".parse().expect("a literal header"));
        return response;
    }
    let authority = match authorize(&state, run, &headers).await {
        Ok(authority) => authority,
        Err(response) => return response,
    };
    let classification = match native::classify(state.bot_delegate_port(), &authority).await {
        Ok(value) => value,
        Err(error) => return refuse(StatusCode::SERVICE_UNAVAILABLE, error),
    };
    let is_bot = matches!(classification, RunToolAuthority::Bot { .. });
    let limit = if is_bot {
        4 * 1024 * 1024
    } else {
        MESSAGE_LIMIT
    };
    if body.len() > limit {
        return jsonrpc_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            None,
            ErrorData::new(
                ErrorCode::INVALID_REQUEST,
                "The message exceeds the Run endpoint's limit.",
                None,
            ),
        );
    }
    let message: ClientJsonRpcMessage = match serde_json::from_slice(&body) {
        Ok(message) => message,
        Err(_) => {
            return jsonrpc_error(
                StatusCode::BAD_REQUEST,
                None,
                ErrorData::new(
                    ErrorCode::PARSE_ERROR,
                    "The body is not one JSON-RPC 2.0 message.",
                    None,
                ),
            );
        }
    };
    let request = match message {
        JsonRpcMessage::Request(request) => request,
        // A notification, or an answer to a request this server never
        // made: accepted, nothing to say.
        JsonRpcMessage::Notification(_)
        | JsonRpcMessage::Response(_)
        | JsonRpcMessage::Error(_) => {
            return StatusCode::ACCEPTED.into_response();
        }
    };
    let id = request.id;
    match request.request {
        ClientRequest::InitializeRequest(initialize_request) => answer(
            id,
            ServerResult::InitializeResult(initialize(
                &initialize_request.params.protocol_version,
                is_bot,
            )),
        ),
        ClientRequest::PingRequest(_) => answer(id, ServerResult::empty(())),
        ClientRequest::ListToolsRequest(_) => answer(
            id,
            ServerResult::ListToolsResult(ListToolsResult::with_all_items(if is_bot {
                native::definitions()
            } else {
                tools::definitions()
            })),
        ),
        ClientRequest::CallToolRequest(call) => answer(
            id,
            ServerResult::CallToolResult(match classification {
                RunToolAuthority::Bot { provenance, policy } => {
                    native::call(&state, authority, provenance, policy, call.params).await
                }
                RunToolAuthority::Ordinary {} => tools::call(&state, authority, call.params).await,
            }),
        ),
        other => jsonrpc_error(
            StatusCode::OK,
            Some(id),
            ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                format!(
                    "The run endpoint serves initialize, ping, tools/list and tools/call; not {}.",
                    other.method()
                ),
                None,
            ),
        ),
    }
}
