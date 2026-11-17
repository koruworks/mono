// SPDX-License-Identifier: MPL-2.0

//! Finite private delegate-to-AI decision enrollment transport. Its own
//! credential domain (`Koru-Decision`), never the Routine credential, the
//! ordinary session bearer, renewal, IPC or retry. One request in flight; one
//! shared five-second deadline; after a physical attempt every failure is
//! unknown completion and none proves that an enrollment is missing.
use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering};
use std::time::Instant;

use crate::routine_bridge::{
    AfterSend, BeforeSend, Permit, RoutineBridgeTransportError, before_remaining, bounded_deadline,
    origin, read_bounded,
};
use koru_delegate_api::decision_bridge::*;
use koru_delegate_core::children::{
    BotChildWaitEnrollmentReply, EnrollBotChildWaitCleanup, ReadBotChildWaitCleanup,
};
use koru_delegate_core::decisions::{
    BotWaitEnrollmentReply, EnrollBotWaitCleanup, ReadBotWaitCleanup,
};
use serde::{Serialize, de::DeserializeOwned};

// The shared transport helpers enforce the Routine bridge's frame and deadline
// bounds; the decision contract fixes the same values.
const _: () =
    assert!(DECISION_BRIDGE_BODY_LIMIT == koru_delegate_api::routines::ROUTINE_BRIDGE_BODY_LIMIT);
const _: () = assert!(
    DECISION_BRIDGE_DEADLINE.as_millis()
        == koru_delegate_api::routines::ROUTINE_BRIDGE_DEADLINE.as_millis()
);

/// Content-free local errors, deliberately not wire DTOs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionBridgeTransportError {
    NotSent(BeforeSend),
    Unknown(AfterSend),
}
impl From<RoutineBridgeTransportError> for DecisionBridgeTransportError {
    fn from(error: RoutineBridgeTransportError) -> Self {
        match error {
            RoutineBridgeTransportError::NotSent(reason) => Self::NotSent(reason),
            RoutineBridgeTransportError::Unknown(reason) => Self::Unknown(reason),
        }
    }
}
impl std::fmt::Display for DecisionBridgeTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSent(reason) => write!(f, "decision bridge request was not sent: {reason:?}"),
            Self::Unknown(reason) => write!(f, "decision bridge completion is unknown: {reason:?}"),
        }
    }
}
impl std::error::Error for DecisionBridgeTransportError {}
pub type DecisionBridgeTransportResult<T> = Result<T, DecisionBridgeTransportError>;

#[derive(Clone)]
pub struct DecisionBridgeClient {
    inner: Arc<Inner>,
}
struct Inner {
    origin: reqwest::Url,
    http: reqwest::blocking::Client,
    authorization: reqwest::header::HeaderValue,
    in_flight: AtomicBool,
}
impl std::fmt::Debug for DecisionBridgeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DecisionBridgeClient([REDACTED])")
    }
}

impl crate::Client {
    /// Explicit trusted-bootstrap construction for the decision domain only.
    pub fn decision_bridge(
        &self,
        authority: DecisionBridgeAuthority,
    ) -> DecisionBridgeTransportResult<DecisionBridgeClient> {
        let unavailable = DecisionBridgeTransportError::NotSent(BeforeSend::Unavailable);
        let gateway = self.gateway.as_ref().ok_or(unavailable)?;
        let origin = {
            let snapshot = gateway.current.read().map_err(|_| unavailable)?;
            origin(&snapshot.api_endpoint)?
        };
        let mut authorization =
            reqwest::header::HeaderValue::from_str(&authority.authorization_header())
                .map_err(|_| unavailable)?;
        authorization.set_sensitive(true);
        let http = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .http1_only()
            .pool_max_idle_per_host(0)
            .connect_timeout(DECISION_BRIDGE_DEADLINE)
            .timeout(DECISION_BRIDGE_DEADLINE)
            .build()
            .map_err(|_| unavailable)?;
        Ok(DecisionBridgeClient {
            inner: Arc::new(Inner {
                origin,
                http,
                authorization,
                in_flight: AtomicBool::new(false),
            }),
        })
    }
}

impl DecisionBridgeClient {
    /// One enrollment attempt under the caller's absolute deadline (capped at
    /// the shared five seconds). Enroll never answers Missing.
    pub fn enroll_until(
        &self,
        request: &DecisionBridgeEnvelope<EnrollBotWaitCleanup>,
        deadline: Instant,
    ) -> DecisionBridgeTransportResult<DecisionBridgeEnvelope<BotWaitEnrollmentReply>> {
        self.call_until(DECISION_ENROLL_ROUTE, request, deadline)
    }

    /// The exact historical enrollment read. Missing here is a real answer only
    /// because the server read the exact subject successfully.
    pub fn read_until(
        &self,
        request: &DecisionBridgeEnvelope<ReadBotWaitCleanup>,
        deadline: Instant,
    ) -> DecisionBridgeTransportResult<DecisionBridgeEnvelope<BotWaitEnrollmentReply>> {
        self.call_until(DECISION_READ_ROUTE, request, deadline)
    }

    /// One enrollment attempt for a child wait (M6), on its own route with
    /// its own payload type, under the same credential and deadline. Enroll
    /// never answers Missing.
    pub fn enroll_child_until(
        &self,
        request: &DecisionBridgeEnvelope<EnrollBotChildWaitCleanup>,
        deadline: Instant,
    ) -> DecisionBridgeTransportResult<DecisionBridgeEnvelope<BotChildWaitEnrollmentReply>> {
        self.call_until(CHILD_ENROLL_ROUTE, request, deadline)
    }

    /// The exact historical enrollment read of a child wait. Missing here is
    /// a real answer only because the server read the exact subject.
    pub fn read_child_until(
        &self,
        request: &DecisionBridgeEnvelope<ReadBotChildWaitCleanup>,
        deadline: Instant,
    ) -> DecisionBridgeTransportResult<DecisionBridgeEnvelope<BotChildWaitEnrollmentReply>> {
        self.call_until(CHILD_READ_ROUTE, request, deadline)
    }

    fn call_until<Q, R>(
        &self,
        path: &'static str,
        request: &DecisionBridgeEnvelope<Q>,
        caller_deadline: Instant,
    ) -> DecisionBridgeTransportResult<DecisionBridgeEnvelope<R>>
    where
        Q: DecisionBridgePayload + Serialize,
        R: DecisionBridgeReply<Q> + DeserializeOwned,
    {
        let deadline = bounded_deadline(caller_deadline, Instant::now())?;
        before_remaining(deadline)?;
        self.inner
            .in_flight
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| DecisionBridgeTransportError::NotSent(BeforeSend::Capacity))?;
        // The permit outlives send, body reads, decode and response drop.
        let _permit = Permit(&self.inner.in_flight);
        before_remaining(deadline)?;
        let bytes = request.encode().map_err(|error| {
            DecisionBridgeTransportError::NotSent(match error {
                DecisionBridgeCodecError::TooLarge => BeforeSend::ContextTooLarge,
                _ => BeforeSend::InvalidRequest,
            })
        })?;
        let remaining = before_remaining(deadline)?;
        let mut endpoint = self.inner.origin.clone();
        endpoint.set_path(path);
        let mut response = self
            .inner
            .http
            .post(endpoint)
            .timeout(remaining)
            .header(
                reqwest::header::AUTHORIZATION,
                self.inner.authorization.clone(),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes)
            .send()
            .map_err(|error| {
                DecisionBridgeTransportError::Unknown(
                    if error.is_timeout() || Instant::now() >= deadline {
                        AfterSend::Deadline
                    } else {
                        AfterSend::Transport
                    },
                )
            })?;
        if Instant::now() >= deadline {
            return Err(DecisionBridgeTransportError::Unknown(AfterSend::Deadline));
        }
        let status = response.status();
        if status != reqwest::StatusCode::OK {
            return Err(DecisionBridgeTransportError::Unknown(
                if status == reqwest::StatusCode::UNAUTHORIZED {
                    AfterSend::Unauthorized
                } else if status.is_redirection() {
                    AfterSend::Redirect
                } else {
                    AfterSend::UnexpectedStatus(status.as_u16())
                },
            ));
        }
        let length = response.content_length();
        let body = read_bounded(&mut response, length, deadline)?;
        let reply = DecisionBridgeEnvelope::<R>::decode(&body)
            .map_err(|_| DecisionBridgeTransportError::Unknown(AfterSend::InvalidResponse))?;
        reply
            .validate_reply_to(request)
            .map_err(|_| DecisionBridgeTransportError::Unknown(AfterSend::MismatchedResponse))?;
        drop(response);
        if Instant::now() >= deadline {
            return Err(DecisionBridgeTransportError::Unknown(AfterSend::Deadline));
        }
        Ok(reply)
    }
}

#[cfg(test)]
mod tests;
