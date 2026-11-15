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
use koru_delegate_api::routines::ROUTINE_BRIDGE_DEADLINE;
use koru_delegate_core::decisions::{
    BotWaitEnrollmentKey, BotWaitEnrollmentReply, BotWaitRefusal, DecisionError, DecisionValidate,
    EnrollBotWaitCleanup, ReadBotWaitCleanup, decode_decision_frame, encode_decision_frame,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::routine_bridge::{HTTP_CAPACITY, ResponseAllocation, framing, read_body, remaining};

pub const DECISION_ENROLL_ROUTE: &str = "/private/bots/decisions/enroll-wait-cleanup";
pub const DECISION_READ_ROUTE: &str = "/private/bots/decisions/read-wait-cleanup";

/// The decision domain's bridge credential check. The delegate API author's
/// domain-scoped authority (derived from the existing parent under its own
/// context) implements this in composition; until it does, nothing is
/// installed and every decision request answers 401.
pub trait DecisionBridgeCredential: Send + Sync {
    /// Exact fixed-length comparison of one Authorization header value,
    /// before any body is read.
    fn matches_authorization(&self, candidate: &[u8]) -> bool;
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

/// Same outer shape as the Routine bridge: version 1, a canonical v4 request
/// id the AI owner keys its phases by, and one closed payload.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionBridgeEnvelope<T> {
    #[serde(deserialize_with = "version_one")]
    version: u8,
    #[serde(deserialize_with = "request_uuid")]
    request_id: Uuid,
    payload: T,
}
impl<T: DecisionValidate> DecisionValidate for DecisionBridgeEnvelope<T> {
    fn validate(&self) -> Result<(), DecisionError> {
        if self.version != 1 || !v4(self.request_id) {
            return Err(DecisionError::Invalid);
        }
        self.payload.validate()
    }
}
fn v4(id: Uuid) -> bool {
    id.get_version() == Some(uuid::Version::Random) && id.get_variant() == uuid::Variant::RFC4122
}
fn version_one<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    let version = u8::deserialize(d)?;
    if version == 1 {
        Ok(version)
    } else {
        Err(serde::de::Error::custom(
            "unsupported decision bridge version",
        ))
    }
}
fn request_uuid<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Uuid, D::Error> {
    let text = String::deserialize(d)?;
    let id = Uuid::parse_str(&text)
        .map_err(|_| serde::de::Error::custom("invalid decision request UUID"))?;
    if !v4(id) || id.to_string() != text {
        return Err(serde::de::Error::custom("invalid decision request UUID"));
    }
    Ok(id)
}

/// The two closed requests. Only Read may ever be answered Missing.
trait DecisionRequest: DeserializeOwned + DecisionValidate + Clone {
    const READ: bool;
    fn key(&self) -> &BotWaitEnrollmentKey;
}
impl DecisionRequest for EnrollBotWaitCleanup {
    const READ: bool = false;
    fn key(&self) -> &BotWaitEnrollmentKey {
        &self.key
    }
}
impl DecisionRequest for ReadBotWaitCleanup {
    const READ: bool = true;
    fn key(&self) -> &BotWaitEnrollmentKey {
        &self.key
    }
}

async fn handle(State(state): State<Arc<HttpState>>, request: Request) -> Response {
    let started = Instant::now();
    let Some(deadline) = started.checked_add(ROUTINE_BRIDGE_DEADLINE) else {
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

async fn dispatch<Q: DecisionRequest>(
    worker: Option<&AiWorkerClient>,
    bytes: Vec<u8>,
    deadline: Instant,
    start: Starter<Q>,
) -> Result<Vec<u8>, StatusCode> {
    remaining(deadline)?;
    let request =
        decode_decision_frame::<DecisionBridgeEnvelope<Q>>(&bytes).map_err(
            |error| match error {
                DecisionError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
                DecisionError::Invalid => StatusCode::BAD_REQUEST,
            },
        )?;
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
    if !payload.matches_request(request.payload.key(), Q::READ) {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let reply = encode_decision_frame(&DecisionBridgeEnvelope {
        version: 1,
        request_id: request.request_id,
        payload,
    })
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    remaining(deadline)?;
    Ok(reply)
}

#[cfg(test)]
mod tests;
