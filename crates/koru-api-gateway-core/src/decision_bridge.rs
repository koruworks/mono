// SPDX-License-Identifier: MPL-2.0
//! Authenticated, bounded private decision bridge over the concrete AI owner
//! (P4.2 contract section 6, O115: the contract's literal route paths).
//!
//! This is its own credential domain. The Routine bridge credential is never
//! accepted here: it lives in another slot of another type, and nothing
//! implements [`DecisionBridgeCredential`] for it. A Run or model bearer
//! cannot reach a handler either: the exact header compare happens before any
//! body is polled. The finite HTTP transport, 4 MiB full-frame bound and one
//! shared five-second deadline are the Routine bridge's own helpers.
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use koru_ai_core::Cancellation;
use koru_ai_server::{AiWorkerClient, RoutineCall, RoutineCallContext};
// One copy of the bridge contract: routes, envelope, codec and reply
// correlation are the delegate API's, which the host's client also speaks.
use koru_delegate_api::decision_bridge::{
    DECISION_BRIDGE_BODY_LIMIT, DECISION_BRIDGE_DEADLINE, DecisionBridgeAuthority,
    DecisionBridgeCodecError, DecisionBridgeEnvelope, DecisionBridgePayload, DecisionBridgeReply,
};
pub use koru_delegate_api::decision_bridge::{DECISION_ENROLL_ROUTE, DECISION_READ_ROUTE};
use koru_delegate_core::decisions::{BotWaitEnrollmentReply, BotWaitRefusal};
use serde::de::DeserializeOwned;
use tokio::sync::Semaphore;

use crate::routine_bridge::{HTTP_CAPACITY, ResponseAllocation, framing, read_body, remaining};

// The shared HTTP body reader enforces the Routine bridge's frame bound; the
// decision contract fixes the same value.
const _: () =
    assert!(DECISION_BRIDGE_BODY_LIMIT == koru_delegate_api::routines::ROUTINE_BRIDGE_BODY_LIMIT);

/// The decision domain's bridge credential check, implemented by the delegate
/// API's domain-scoped authority below and installed by gateway composition.
/// Until one is installed, every decision request answers 401.
pub trait DecisionBridgeCredential: Send + Sync {
    /// Exact fixed-length comparison of one Authorization header value,
    /// before any body is read.
    fn matches_authorization(&self, candidate: &[u8]) -> bool;
}

/// The delegate API's decision-domain authority: derived from the bootstrap
/// parent under `koru.bot.decision.bridge.v1`, header scheme `Koru-Decision`.
/// The Routine authority has no implementation of this trait.
impl DecisionBridgeCredential for DecisionBridgeAuthority {
    fn matches_authorization(&self, candidate: &[u8]) -> bool {
        DecisionBridgeAuthority::matches_authorization(self, candidate)
    }
}

pub(crate) struct HttpState {
    credential: OnceLock<Arc<dyn DecisionBridgeCredential>>,
    worker: Option<AiWorkerClient>,
    /// Request and application response-buffer capacity only, never AI
    /// physical work; that is the AI worker's own lane permit.
    slots: Arc<Semaphore>,
}
impl HttpState {
    pub(crate) fn new(worker: Option<AiWorkerClient>) -> Self {
        Self {
            credential: OnceLock::new(),
            worker,
            slots: Arc::new(Semaphore::new(HTTP_CAPACITY)),
        }
    }
    pub(crate) fn install(&self, credential: Arc<dyn DecisionBridgeCredential>) -> bool {
        self.credential.set(credential).is_ok()
    }
}

pub(crate) fn is_endpoint(path: &str) -> bool {
    matches!(path, DECISION_ENROLL_ROUTE | DECISION_READ_ROUTE)
}

fn routes() -> Router<Arc<HttpState>> {
    Router::new()
        .route(DECISION_ENROLL_ROUTE, any(handle))
        .route(DECISION_READ_ROUTE, any(handle))
}

pub(crate) fn router(state: Arc<HttpState>) -> Router<Arc<crate::GatewayState>> {
    routes().with_state(state)
}

async fn handle(State(state): State<Arc<HttpState>>, request: Request) -> Response {
    let started = Instant::now();
    let Some(deadline) = started.checked_add(DECISION_BRIDGE_DEADLINE) else {
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

fn authorized(state: &HttpState, headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let Some(value) = values.next() else {
        return false;
    };
    values.next().is_none()
        && state
            .credential
            .get()
            .is_some_and(|credential| credential.matches_authorization(value.as_bytes()))
}

async fn handle_until(
    state: &Arc<HttpState>,
    request: Request,
    deadline: Instant,
) -> Result<Bytes, StatusCode> {
    // No Json/Bytes extractor: nothing is read before the capability check.
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
    // HTTP 200 may carry a typed refusal; a timeout is 504, never Missing.
    let reply = match path.as_str() {
        DECISION_ENROLL_ROUTE => {
            dispatch(
                state.worker.as_ref(),
                bytes,
                deadline,
                AiWorkerClient::enroll_bot_wait_cleanup,
            )
            .await
        }
        DECISION_READ_ROUTE => {
            dispatch(
                state.worker.as_ref(),
                bytes,
                deadline,
                AiWorkerClient::read_bot_wait_cleanup,
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

type Starter<Q> = fn(
    &AiWorkerClient,
    RoutineCallContext,
    Q,
)
    -> Result<RoutineCall<BotWaitEnrollmentReply, BotWaitRefusal>, BotWaitRefusal>;

/// One closed request type (Enroll or Read); the delegate API's reply
/// correlation decides which answers it may receive (only Read: Missing).
async fn dispatch<Q: DecisionBridgePayload + DeserializeOwned + Clone>(
    worker: Option<&AiWorkerClient>,
    bytes: Vec<u8>,
    deadline: Instant,
    start: Starter<Q>,
) -> Result<Vec<u8>, StatusCode>
where
    BotWaitEnrollmentReply: DecisionBridgeReply<Q>,
{
    remaining(deadline)?;
    let request = DecisionBridgeEnvelope::<Q>::decode(&bytes).map_err(|error| match error {
        DecisionBridgeCodecError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        DecisionBridgeCodecError::Invalid | DecisionBridgeCodecError::Mismatch => {
            StatusCode::BAD_REQUEST
        }
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
                    .unwrap_or_else(|code| BotWaitEnrollmentReply::Refused { code }),
                Err(code) => BotWaitEnrollmentReply::Refused { code },
            }
        }
        None => BotWaitEnrollmentReply::Refused {
            code: BotWaitRefusal::Unavailable,
        },
    };
    remaining(deadline)?;
    // Never send a reply that does not answer this exact request: Enroll
    // cannot turn absence into Missing, and no other key can be answered.
    let reply = DecisionBridgeEnvelope::reply_to(&request, payload)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .encode()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    remaining(deadline)?;
    Ok(reply)
}

#[cfg(test)]
mod tests;
