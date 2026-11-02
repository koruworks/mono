// SPDX-License-Identifier: MPL-2.0

//! Authorized HTTP/SSE projection of the typed public session catalog.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, MatchedPath, Path, Query, State};
use axum::http::{StatusCode, request::Request};
use axum::middleware::Next;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use futures_util::stream;
use koru_session_api::{RouteId, route_by_id};
use koru_session_protocol::{AttachmentId, PendingInput, RawCommand, SessionId};

use super::{BridgeError, GatewayPrincipal, GatewayState, SessionBridge, failure};

pub(super) fn routes() -> Router<Arc<GatewayState>> {
    Router::new()
        .route(
            route_by_id(RouteId::Create).path,
            post(create).merge(get(list)),
        )
        .route(
            route_by_id(RouteId::Get).path,
            get(one).merge(delete(terminate)),
        )
        .route(
            route_by_id(RouteId::Attach).path,
            post(attach).merge(delete(detach)),
        )
        .route(route_by_id(RouteId::Snapshot).path, post(snapshot))
        .route(route_by_id(RouteId::Scrollback).path, post(scrollback))
        .route(route_by_id(RouteId::WriteBytes).path, post(write_bytes))
        .route(route_by_id(RouteId::WriteText).path, post(write_text))
        .route(route_by_id(RouteId::SendKey).path, post(send_key))
        .route(route_by_id(RouteId::Paste).path, post(paste))
        .route(
            route_by_id(RouteId::UploadAttachment).path,
            post(upload_attachment).merge(get(list_attachments)),
        )
        .route(
            route_by_id(RouteId::RemoveAttachment).path,
            delete(remove_attachment),
        )
        .route(
            route_by_id(RouteId::SendPendingInput).path,
            post(send_pending),
        )
        .route(route_by_id(RouteId::Interrupt).path, post(interrupt))
        .route(route_by_id(RouteId::Resize).path, put(resize))
        .route(route_by_id(RouteId::Subscribe).path, get(subscribe))
        // The public representation is bounded before JSON extraction. The
        // rooted attachment store applies the stricter decoded-byte policy.
        .layer(axum::extract::DefaultBodyLimit::max(12 * 1024 * 1024))
        .route_layer(axum::middleware::from_fn(authorize))
}

async fn authorize(
    Extension(matched): Extension<MatchedPath>,
    Extension(principal): Extension<GatewayPrincipal>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let method = match *request.method() {
        axum::http::Method::GET => koru_session_api::Method::Get,
        axum::http::Method::POST => koru_session_api::Method::Post,
        axum::http::Method::PUT => koru_session_api::Method::Put,
        axum::http::Method::DELETE => koru_session_api::Method::Delete,
        _ => return next.run(request).await,
    };
    let Some(route) = koru_session_api::route_for(method, matched.as_str()) else {
        return next.run(request).await;
    };
    match koru_session_api::authorize(route, &principal.scopes) {
        Ok(()) => next.run(request).await,
        Err(error) => gateway_failure(
            error,
            "this session does not hold every scope the session operation requires",
        ),
    }
}

// Handlers return Axum's response directly; boxing the capability error would
// add allocation and unwrap noise at every route for a value built only here.
#[allow(clippy::result_large_err)]
fn bridge(state: &GatewayState) -> Result<&SessionBridge, Response> {
    state.session_bridge().ok_or_else(|| {
        gateway_failure(
            koru_session_api::GatewayError::CapabilityUnavailable,
            "the Agent-owned session host is unavailable",
        )
    })
}

async fn create(
    State(state): State<Arc<GatewayState>>,
    request: Result<Json<koru_session_api::CreateSessionRequest>, JsonRejection>,
) -> Response {
    let bridge = match bridge(&state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    result(bridge.create(request).await)
}

async fn list(
    State(state): State<Arc<GatewayState>>,
    Query(request): Query<koru_session_api::ListSessionsRequest>,
) -> Response {
    let bridge = match bridge(&state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    match bridge.list().await {
        Ok(mut sessions) => {
            if request.parent_session_id.is_some() {
                sessions.retain(|session| session.parent_session_id == request.parent_session_id);
            }
            Json(koru_session_api::SessionList { sessions }).into_response()
        }
        Err(error) => bridge_failure(error),
    }
}

async fn one(State(state): State<Arc<GatewayState>>, Path(id): Path<SessionId>) -> Response {
    let bridge = match bridge(&state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    result(bridge.get(id).await)
}

async fn attach(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    request: Result<Json<koru_session_api::AttachSessionRequest>, JsonRejection>,
) -> Response {
    let bridge = match bridge(&state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    result(
        bridge
            .attach(
                id,
                request.viewport_generation,
                request.columns,
                request.overscan_rows,
            )
            .await,
    )
}

async fn detach(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    request: Result<Json<koru_session_api::DetachSessionRequest>, JsonRejection>,
) -> Response {
    let bridge = match bridge(&state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    empty(bridge.detach(id, request.controller_lease).await)
}

async fn snapshot(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    request: Result<Json<koru_session_api::SnapshotRequest>, JsonRejection>,
) -> Response {
    let bridge = match bridge(&state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match bridge.snapshot(id, request).await {
        Ok(observation) => Json(koru_session_api::SnapshotResponse {
            snapshot: observation.snapshot,
            resume: koru_session_api::SubscribeRequest {
                after_event: Some(observation.event_sequence),
                after_output: Some(observation.output_sequence),
            },
        })
        .into_response(),
        Err(error) => bridge_failure(error),
    }
}

async fn scrollback(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    request: Result<Json<koru_session_api::ScrollbackRequest>, JsonRejection>,
) -> Response {
    let bridge = match bridge(&state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match bridge.scrollback(id, request).await {
        Ok(page) => Json(koru_session_api::ScrollbackResponse { page }).into_response(),
        Err(error) => bridge_failure(error),
    }
}

async fn write_bytes(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    request: Result<Json<koru_session_api::WriteBytesRequest>, JsonRejection>,
) -> Response {
    let bridge = match bridge(&state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match bridge
        .write_bytes(
            request.idempotency_key,
            id,
            request.controller_lease,
            request.data_base64,
        )
        .await
    {
        Ok(()) => Json(koru_session_api::CommandReceipt {
            idempotency_key: request.idempotency_key,
        })
        .into_response(),
        Err(error) => bridge_failure(error),
    }
}

async fn write_text(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    request: Result<Json<koru_session_api::WriteTextRequest>, JsonRejection>,
) -> Response {
    if let Err(response) = bridge(&state) {
        return response;
    }
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    command(
        &state,
        id,
        request.idempotency_key,
        request.controller_lease,
        RawCommand::WriteText { text: request.text },
    )
    .await
}

async fn send_key(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    request: Result<Json<koru_session_api::SendKeyRequest>, JsonRejection>,
) -> Response {
    if let Err(response) = bridge(&state) {
        return response;
    }
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    command(
        &state,
        id,
        request.idempotency_key,
        request.controller_lease,
        RawCommand::SendKey { key: request.key },
    )
    .await
}

async fn paste(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    request: Result<Json<koru_session_api::PasteTextRequest>, JsonRejection>,
) -> Response {
    if let Err(response) = bridge(&state) {
        return response;
    }
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    command(
        &state,
        id,
        request.idempotency_key,
        request.controller_lease,
        RawCommand::PasteText { text: request.text },
    )
    .await
}

async fn interrupt(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    request: Result<Json<koru_session_api::InterruptRequest>, JsonRejection>,
) -> Response {
    if let Err(response) = bridge(&state) {
        return response;
    }
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    command(
        &state,
        id,
        request.idempotency_key,
        request.controller_lease,
        RawCommand::Interrupt,
    )
    .await
}

async fn resize(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    request: Result<Json<koru_session_api::ResizeRequest>, JsonRejection>,
) -> Response {
    if let Err(response) = bridge(&state) {
        return response;
    }
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    command(
        &state,
        id,
        request.idempotency_key,
        request.controller_lease,
        RawCommand::Resize { size: request.size },
    )
    .await
}

async fn terminate(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    request: Result<Json<koru_session_api::TerminateRequest>, JsonRejection>,
) -> Response {
    if let Err(response) = bridge(&state) {
        return response;
    }
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    command(
        &state,
        id,
        request.idempotency_key,
        request.controller_lease,
        RawCommand::Terminate,
    )
    .await
}

async fn command(
    state: &GatewayState,
    session_id: SessionId,
    idempotency_key: koru_session_protocol::IdempotencyKey,
    lease: koru_session_protocol::ControllerLeaseToken,
    action: RawCommand,
) -> Response {
    let bridge = match bridge(state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    let terminating = matches!(action, RawCommand::Terminate);
    match bridge
        .command(idempotency_key, session_id, lease, action)
        .await
    {
        Ok(()) => {
            if terminating {
                bridge.forget_controller(session_id, lease);
            }
            Json(koru_session_api::CommandReceipt { idempotency_key }).into_response()
        }
        Err(error) => bridge_failure(error),
    }
}

async fn upload_attachment(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    request: Result<Json<koru_session_api::UploadAttachmentRequest>, JsonRejection>,
) -> Response {
    let bridge = match bridge(&state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    result(
        bridge
            .upload(
                id,
                request.controller_lease,
                request.attachment_id,
                request.data_base64,
            )
            .await,
    )
}

async fn list_attachments(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
) -> Response {
    let bridge = match bridge(&state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    match bridge.attachments(id).await {
        Ok(attachments) => Json(koru_session_api::AttachmentList { attachments }).into_response(),
        Err(error) => bridge_failure(error),
    }
}

async fn remove_attachment(
    State(state): State<Arc<GatewayState>>,
    Path((session_id, attachment_id)): Path<(SessionId, AttachmentId)>,
    request: Result<Json<koru_session_api::RemoveAttachmentRequest>, JsonRejection>,
) -> Response {
    let bridge = match bridge(&state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    result(
        bridge
            .remove_attachment(session_id, request.controller_lease, attachment_id)
            .await,
    )
}

async fn send_pending(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    request: Result<Json<koru_session_api::SendPendingInputRequest>, JsonRejection>,
) -> Response {
    let bridge = match bridge(&state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    let request = match body(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    result(
        bridge
            .send_pending(
                id,
                request.controller_lease,
                PendingInput {
                    idempotency_key: request.idempotency_key,
                    text: request.text,
                    attachment_ids: request.attachment_ids,
                    mode: None,
                    fast: None,
                },
            )
            .await,
    )
}

async fn subscribe(
    State(state): State<Arc<GatewayState>>,
    Path(id): Path<SessionId>,
    Query(request): Query<koru_session_api::SubscribeRequest>,
) -> Response {
    let bridge = match bridge(&state) {
        Ok(bridge) => bridge,
        Err(response) => return response,
    };
    let subscription = match bridge.subscribe(id, request).await {
        Ok(subscription) => subscription,
        Err(error) => return bridge_failure(error),
    };
    let events = stream::unfold(subscription, |mut subscription| async move {
        let item = subscription.next().await?;
        let event = Event::default()
            .event("session_stream_item")
            .json_data(item)
            .expect("session stream DTO always serializes");
        Some((Ok::<_, Infallible>(event), subscription))
    });
    Sse::new(events).into_response()
}

fn result<T: serde::Serialize>(value: Result<T, BridgeError>) -> Response {
    match value {
        Ok(value) => Json(value).into_response(),
        Err(error) => bridge_failure(error),
    }
}

// The rejected body is already the handler's final response. Keeping it direct
// makes every extractor branch allocation-free and visibly identical.
#[allow(clippy::result_large_err)]
fn body<T>(value: Result<Json<T>, JsonRejection>) -> Result<T, Response> {
    value
        .map(|Json(value)| value)
        .map_err(|_| session_failure(koru_session_protocol::ErrorCode::InvalidRequest))
}

fn empty(value: Result<(), BridgeError>) -> Response {
    match value {
        Ok(()) => Json(koru_session_api::EmptyResponse {}).into_response(),
        Err(error) => bridge_failure(error),
    }
}

fn bridge_failure(error: BridgeError) -> Response {
    match error {
        BridgeError::Session(error) => session_failure_with_message(error),
        BridgeError::Saturated => gateway_failure(
            koru_session_api::GatewayError::GatewaySaturated,
            "the bounded session worker lane is full",
        ),
        BridgeError::TimedOut => gateway_failure(
            koru_session_api::GatewayError::DeadlineExceeded,
            "the Agent-owned session host did not answer before the deadline",
        ),
        BridgeError::Unavailable => gateway_failure(
            koru_session_api::GatewayError::ServerUnavailable,
            "the Agent-owned session host is unavailable",
        ),
        BridgeError::Internal => session_failure(koru_session_protocol::ErrorCode::Internal),
    }
}

fn session_failure(code: koru_session_protocol::ErrorCode) -> Response {
    session_failure_with_message(koru_session_protocol::SessionFailure {
        message: code.as_str().to_owned(),
        code,
    })
}

fn session_failure_with_message(error: koru_session_protocol::SessionFailure) -> Response {
    let status = koru_session_api::public_status(&error.code);
    failure(
        StatusCode::from_u16(status.0).expect("catalogued HTTP status is valid"),
        error.code.as_str(),
        &error.message,
    )
}

fn gateway_failure(error: koru_session_api::GatewayError, message: &str) -> Response {
    failure(
        StatusCode::from_u16(error.status().0).expect("catalogued HTTP status is valid"),
        error.as_str(),
        message,
    )
}
