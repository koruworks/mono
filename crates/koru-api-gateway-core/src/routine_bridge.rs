// SPDX-License-Identifier: MPL-2.0
//! Authenticated, bounded private HTTP over the concrete AI occurrence owner.
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use futures_util::StreamExt;
use koru_ai_core::Cancellation;
use koru_ai_server::{AiWorkerClient, RoutineCall, RoutineCallContext};
use koru_delegate_api::routines::*;
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const HTTP_CAPACITY: usize = 8;

/// The permit follows the application buffer through Body and every Bytes
/// clone. There is no TTL: retained memory must retain its allocation slot.
/// Field order drops the buffer before releasing its permit.
struct ResponseAllocation {
    bytes: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}
impl AsRef<[u8]> for ResponseAllocation {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

pub(crate) struct HttpState {
    authority: OnceLock<RoutineBridgeAuthority>,
    worker: Option<AiWorkerClient>,
    /// Request and application response-buffer capacity, never AI physical work
    /// or socket/kernel buffering. Encoded responses retain their own slot.
    slots: Arc<Semaphore>,
    #[cfg(test)]
    test_budget: std::time::Duration,
}
impl Default for HttpState {
    fn default() -> Self {
        Self::new(None)
    }
}
impl HttpState {
    pub(crate) fn new(worker: Option<AiWorkerClient>) -> Self {
        Self {
            authority: OnceLock::new(),
            worker,
            slots: Arc::new(Semaphore::new(HTTP_CAPACITY)),
            #[cfg(test)]
            test_budget: ROUTINE_BRIDGE_DEADLINE,
        }
    }
    pub(crate) fn install(&self, authority: RoutineBridgeAuthority) -> bool {
        self.authority.set(authority).is_ok()
    }

    fn budget(&self) -> std::time::Duration {
        #[cfg(test)]
        {
            self.test_budget
        }
        #[cfg(not(test))]
        {
            ROUTINE_BRIDGE_DEADLINE
        }
    }
}

pub(crate) fn is_endpoint(path: &str) -> bool {
    matches!(
        path,
        ROUTINE_PREPARE_ROUTE
            | ROUTINE_PRECLAIM_ROUTE
            | ROUTINE_PREPARATION_ROUTE
            | ROUTINE_FINALIZE_ROUTE
            | ROUTINE_LINK_ROUTE
    )
}

fn routes() -> Router<Arc<HttpState>> {
    Router::new()
        .route(ROUTINE_PREPARE_ROUTE, any(handle))
        .route(ROUTINE_PREPARATION_ROUTE, any(handle))
        .route(ROUTINE_FINALIZE_ROUTE, any(handle))
        .route(ROUTINE_LINK_ROUTE, any(handle))
        .route(ROUTINE_PRECLAIM_ROUTE, any(handle))
}

pub(crate) fn router(state: Arc<HttpState>) -> Router<Arc<crate::GatewayState>> {
    routes().with_state(state)
}

#[cfg(feature = "fixtures")]
pub(crate) fn fixture_router(
    authority: RoutineBridgeAuthority,
    worker: Option<AiWorkerClient>,
) -> Router {
    let state = Arc::new(HttpState::new(worker));
    assert!(state.install(authority));
    routes().with_state(state)
}

async fn handle(State(state): State<Arc<HttpState>>, request: Request) -> Response {
    let started = Instant::now();
    let Some(deadline) = started.checked_add(state.budget()) else {
        return StatusCode::GATEWAY_TIMEOUT.into_response();
    };
    match handle_until(&state, request, deadline).await {
        Ok(body) => {
            let mut response = Response::new(Body::from(body));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            );
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(status) => {
            let mut response = status.into_response();
            if status == StatusCode::METHOD_NOT_ALLOWED {
                response
                    .headers_mut()
                    .insert(header::ALLOW, axum::http::HeaderValue::from_static("POST"));
            }
            response
        }
    }
}

fn remaining(deadline: Instant) -> Result<(), StatusCode> {
    if Instant::now() >= deadline {
        Err(StatusCode::GATEWAY_TIMEOUT)
    } else {
        Ok(())
    }
}

fn authorized(state: &HttpState, headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let Some(value) = values.next() else {
        return false;
    };
    values.next().is_none()
        && state
            .authority
            .get()
            .is_some_and(|authority| authority.matches_authorization(value.as_bytes()))
}

fn framing(headers: &HeaderMap) -> Result<Option<usize>, StatusCode> {
    let bad = StatusCode::BAD_REQUEST;
    let mut types = headers.get_all(header::CONTENT_TYPE).iter();
    let content_type = types.next().ok_or(bad)?.to_str().map_err(|_| bad)?;
    if types.next().is_some() {
        return Err(bad);
    }
    let mut parts = content_type.split(';').map(str::trim);
    if !parts
        .next()
        .is_some_and(|part| part.eq_ignore_ascii_case("application/json"))
        || parts
            .next()
            .is_some_and(|part| !part.eq_ignore_ascii_case("charset=utf-8"))
        || parts.next().is_some()
        || headers.contains_key(header::CONTENT_ENCODING)
    {
        return Err(bad);
    }
    let mut lengths = headers.get_all(header::CONTENT_LENGTH).iter();
    let length = lengths
        .next()
        .map(|value| {
            let value = value.to_str().map_err(|_| bad)?;
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(bad);
            }
            let length = value.parse::<u64>().map_err(|_| bad)?;
            if length > ROUTINE_BRIDGE_BODY_LIMIT as u64 {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            Ok(length as usize)
        })
        .transpose()?;
    if lengths.next().is_some() {
        return Err(bad);
    }
    let mut transfers = headers.get_all(header::TRANSFER_ENCODING).iter();
    if let Some(value) = transfers.next() {
        if length.is_some()
            || transfers.next().is_some()
            || !value.as_bytes().eq_ignore_ascii_case(b"chunked")
        {
            return Err(bad);
        }
    }
    Ok(length)
}

async fn handle_until(
    state: &Arc<HttpState>,
    request: Request,
    deadline: Instant,
) -> Result<Bytes, StatusCode> {
    // Do not use Json/Bytes extractors: they can read before this capability.
    if !authorized(state, request.headers()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if request.method() != Method::POST {
        return Err(StatusCode::METHOD_NOT_ALLOWED);
    }
    remaining(deadline)?;
    let permit = state
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    if request.uri().query().is_some() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let expected = framing(request.headers())?;
    let path = request.uri().path().to_owned();
    let bytes = read_body(request.into_body(), expected, deadline).await?;
    remaining(deadline)?;
    // Each call keeps the entry deadline and concrete worker-owned physical
    // capacity. HTTP200 may carry a typed refusal, never invented absence.
    let reply = match path.as_str() {
        ROUTINE_PREPARE_ROUTE => {
            dispatch(
                state.worker.as_ref(),
                bytes,
                deadline,
                AiWorkerClient::prepare_routine_occurrence,
                |code| PrepareOccurrenceReply::Refused { code },
            )
            .await
        }
        ROUTINE_PREPARATION_ROUTE => {
            dispatch(
                state.worker.as_ref(),
                bytes,
                deadline,
                AiWorkerClient::read_routine_preparation,
                |code| ReadPreparationReply::Refused { code },
            )
            .await
        }
        ROUTINE_FINALIZE_ROUTE => {
            dispatch(
                state.worker.as_ref(),
                bytes,
                deadline,
                AiWorkerClient::finalize_routine_occurrence,
                |code| FinalizeOccurrenceReply::Refused { code },
            )
            .await
        }
        ROUTINE_LINK_ROUTE => {
            dispatch(
                state.worker.as_ref(),
                bytes,
                deadline,
                AiWorkerClient::read_routine_occurrence_link,
                |code| ReadOccurrenceLinkReply::Refused { code },
            )
            .await
        }
        ROUTINE_PRECLAIM_ROUTE => {
            dispatch(
                state.worker.as_ref(),
                bytes,
                deadline,
                AiWorkerClient::synchronize_routine_preclaim,
                |code| SynchronizeRoutinePreclaimReply::Refused { code },
            )
            .await
        }
        _ => Err(StatusCode::NOT_FOUND),
    }?;
    remaining(deadline)?;
    Ok(Bytes::from_owner(ResponseAllocation {
        bytes: reply,
        _permit: permit,
    }))
}

async fn read_body(
    body: Body,
    expected: Option<usize>,
    deadline: Instant,
) -> Result<Vec<u8>, StatusCode> {
    remaining(deadline)?;
    let mut stream = body.into_data_stream();
    let mut bytes = Vec::new();
    loop {
        let chunk =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), stream.next())
                .await
                .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?;
        remaining(deadline)?;
        let Some(chunk) = chunk else {
            break;
        };
        let chunk = chunk.map_err(|_| StatusCode::BAD_REQUEST)?;
        let take = chunk.len().min(ROUTINE_BRIDGE_BODY_LIMIT + 1 - bytes.len());
        let needed = bytes.len() + take;
        if needed > bytes.capacity() {
            // Amortized copies for tiny chunks, with every requested capacity
            // capped at the single overflow byte. No unauthenticated reserve.
            let target = needed
                .max(bytes.capacity().saturating_mul(2).max(4096))
                .min(ROUTINE_BRIDGE_BODY_LIMIT + 1);
            bytes
                .try_reserve_exact(target - bytes.len())
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        }
        bytes.extend_from_slice(&chunk[..take]);
        if take < chunk.len() || bytes.len() > ROUTINE_BRIDGE_BODY_LIMIT {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
    }
    if expected.is_some_and(|length| length != bytes.len()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    remaining(deadline)?;
    Ok(bytes)
}

type Starter<Q, R> =
    fn(&AiWorkerClient, RoutineCallContext, Q) -> Result<RoutineCall<R>, RoutineBridgeRefusal>;

async fn dispatch<Q, R>(
    worker: Option<&AiWorkerClient>,
    bytes: Vec<u8>,
    deadline: Instant,
    start: Starter<Q, R>,
    refused: fn(RoutineBridgeRefusal) -> R,
) -> Result<Vec<u8>, StatusCode>
where
    Q: RoutineBridgePayload + DeserializeOwned + Clone,
    R: RoutineBridgeReply<Q> + Serialize,
{
    remaining(deadline)?;
    let request = RoutineBridgeEnvelope::<Q>::decode(&bytes).map_err(|error| match error {
        RoutineBridgeCodecError::ContextTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        _ => StatusCode::BAD_REQUEST,
    })?;
    drop(bytes);
    remaining(deadline)?;
    let payload = match worker {
        Some(worker) => {
            let context =
                RoutineCallContext::new(request.request_id, deadline, Cancellation::default());
            match start(worker, context, request.payload.clone()) {
                Ok(call) => tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), call)
                    .await
                    .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
                    .unwrap_or_else(refused),
                Err(code) => refused(code),
            }
        }
        None => refused(RoutineBridgeRefusal::Unavailable),
    };
    remaining(deadline)?;
    let reply = RoutineBridgeEnvelope::reply_to(&request, payload)
        .and_then(|reply| reply.encode())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    remaining(deadline)?;
    Ok(reply)
}

#[cfg(test)]
mod tests;
