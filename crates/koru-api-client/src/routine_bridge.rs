// SPDX-License-Identifier: MPL-2.0

//! Finite private scheduler-to-AI transport. This installs no gateway endpoint
//! or worker action and never borrows ordinary session retry/renewal behavior.
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use koru_delegate_api::routines::*;
use serde::{Serialize, de::DeserializeOwned};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeforeSend {
    Unavailable,
    InvalidEndpoint,
    InvalidRequest,
    ContextTooLarge,
    Capacity,
    Deadline,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterSend {
    Transport,
    Deadline,
    Unauthorized,
    Redirect,
    UnexpectedStatus(u16),
    ContextTooLarge,
    InvalidResponse,
    MismatchedResponse,
}

/// Content-free local errors, deliberately not wire DTOs. After a physical
/// attempt every failure preserves unknown completion. None proves Missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutineBridgeTransportError {
    NotSent(BeforeSend),
    Unknown(AfterSend),
}
impl std::fmt::Display for RoutineBridgeTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSent(reason) => write!(f, "Routine bridge request was not sent: {reason:?}"),
            Self::Unknown(reason) => write!(f, "Routine bridge completion is unknown: {reason:?}"),
        }
    }
}
impl std::error::Error for RoutineBridgeTransportError {}
pub type RoutineBridgeTransportResult<T> = Result<T, RoutineBridgeTransportError>;

#[derive(Clone)]
pub struct RoutineBridgeClient {
    inner: Arc<Inner>,
}
struct Inner {
    origin: reqwest::Url,
    http: reqwest::blocking::Client,
    authorization: reqwest::header::HeaderValue,
    in_flight: AtomicBool,
}
impl std::fmt::Debug for RoutineBridgeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineBridgeClient([REDACTED])")
    }
}

impl crate::Client {
    /// Explicit trusted-bootstrap construction, never a model/client URL.
    /// Snapshot the discovered origin without session renewal, IPC or retry.
    pub fn routine_bridge(
        &self,
        authority: RoutineBridgeAuthority,
    ) -> RoutineBridgeTransportResult<RoutineBridgeClient> {
        let gateway = self
            .gateway
            .as_ref()
            .ok_or(RoutineBridgeTransportError::NotSent(
                BeforeSend::Unavailable,
            ))?;
        let origin = {
            let snapshot = gateway
                .current
                .read()
                .map_err(|_| RoutineBridgeTransportError::NotSent(BeforeSend::Unavailable))?;
            origin(&snapshot.api_endpoint)?
        };
        let mut authorization =
            reqwest::header::HeaderValue::from_str(&authority.authorization_header())
                .map_err(|_| RoutineBridgeTransportError::NotSent(BeforeSend::Unavailable))?;
        authorization.set_sensitive(true);
        let http = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .http1_only()
            .pool_max_idle_per_host(0)
            .connect_timeout(ROUTINE_BRIDGE_DEADLINE)
            .timeout(ROUTINE_BRIDGE_DEADLINE)
            .build()
            .map_err(|_| RoutineBridgeTransportError::NotSent(BeforeSend::Unavailable))?;
        Ok(RoutineBridgeClient {
            inner: Arc::new(Inner {
                origin,
                http,
                authorization,
                in_flight: AtomicBool::new(false),
            }),
        })
    }
}

// Inspect raw authority before URL parsing, which normalizes shorthand integer
// IPv4 hosts and drops explicit default ports. Neither is evidence of a literal
// loopback IP origin with an explicitly supplied nonzero port.
fn origin(value: &str) -> RoutineBridgeTransportResult<reqwest::Url> {
    let invalid = || RoutineBridgeTransportError::NotSent(BeforeSend::InvalidEndpoint);
    let authority = value.strip_prefix("http://").ok_or_else(invalid)?;
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    if authority.contains(['/', '?', '#', '@']) {
        return Err(invalid());
    }
    let (ip, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (host, port) = v6.split_once("]:").ok_or_else(invalid)?;
        (
            IpAddr::V6(host.parse::<Ipv6Addr>().map_err(|_| invalid())?),
            port,
        )
    } else {
        let (host, port) = authority.rsplit_once(':').ok_or_else(invalid)?;
        (
            IpAddr::V4(host.parse::<Ipv4Addr>().map_err(|_| invalid())?),
            port,
        )
    };
    if !ip.is_loopback()
        || port.is_empty()
        || port.len() > 5
        || !port.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(invalid());
    }
    let port = port.parse::<u16>().map_err(|_| invalid())?;
    if port == 0 {
        return Err(invalid());
    }
    let canonical = match ip {
        IpAddr::V4(ip) => format!("http://{ip}:{port}/"),
        IpAddr::V6(ip) => format!("http://[{ip}]:{port}/"),
    };
    reqwest::Url::parse(&canonical).map_err(|_| invalid())
}

struct Permit<'a>(&'a AtomicBool);
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release)
    }
}

impl RoutineBridgeClient {
    pub fn prepare(
        &self,
        request: &RoutineBridgeEnvelope<PrepareOccurrence>,
    ) -> RoutineBridgeTransportResult<RoutineBridgeEnvelope<PrepareOccurrenceReply>> {
        self.call(ROUTINE_PREPARE_ROUTE, request, ROUTINE_BRIDGE_DEADLINE)
    }
    pub fn read_preparation(
        &self,
        request: &RoutineBridgeEnvelope<ReadPreparation>,
    ) -> RoutineBridgeTransportResult<RoutineBridgeEnvelope<ReadPreparationReply>> {
        self.call(ROUTINE_PREPARATION_ROUTE, request, ROUTINE_BRIDGE_DEADLINE)
    }
    pub fn finalize(
        &self,
        request: &RoutineBridgeEnvelope<FinalizeOccurrence>,
    ) -> RoutineBridgeTransportResult<RoutineBridgeEnvelope<FinalizeOccurrenceReply>> {
        self.call(ROUTINE_FINALIZE_ROUTE, request, ROUTINE_BRIDGE_DEADLINE)
    }
    pub fn read_link(
        &self,
        request: &RoutineBridgeEnvelope<ReadOccurrenceLink>,
    ) -> RoutineBridgeTransportResult<RoutineBridgeEnvelope<ReadOccurrenceLinkReply>> {
        self.call(ROUTINE_LINK_ROUTE, request, ROUTINE_BRIDGE_DEADLINE)
    }

    fn call<Q, R>(
        &self,
        path: &'static str,
        request: &RoutineBridgeEnvelope<Q>,
        budget: Duration,
    ) -> RoutineBridgeTransportResult<RoutineBridgeEnvelope<R>>
    where
        Q: RoutineBridgePayload + Serialize,
        R: RoutineBridgeReply<Q> + DeserializeOwned,
    {
        let started = Instant::now();
        let deadline = started
            .checked_add(budget)
            .ok_or(RoutineBridgeTransportError::NotSent(BeforeSend::Deadline))?;
        self.inner
            .in_flight
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| RoutineBridgeTransportError::NotSent(BeforeSend::Capacity))?;
        // The permit outlives send, physical body reads, decode and response
        // destruction. No outer timeout thread can release it early.
        let _permit = Permit(&self.inner.in_flight);
        before_remaining(deadline)?;
        let bytes = request.encode().map_err(|error| {
            RoutineBridgeTransportError::NotSent(match error {
                RoutineBridgeCodecError::ContextTooLarge => BeforeSend::ContextTooLarge,
                _ => BeforeSend::InvalidRequest,
            })
        })?;
        let remaining = before_remaining(deadline)?;
        let mut endpoint = self.inner.origin.clone();
        endpoint.set_path(path);
        // RequestBuilder::timeout is the async total response-body deadline;
        // blocking ClientBuilder::timeout alone also supplies per-read waits.
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
                RoutineBridgeTransportError::Unknown(
                    if error.is_timeout() || Instant::now() >= deadline {
                        AfterSend::Deadline
                    } else {
                        AfterSend::Transport
                    },
                )
            })?;
        if Instant::now() >= deadline {
            return Err(RoutineBridgeTransportError::Unknown(AfterSend::Deadline));
        }
        let status = response.status();
        if status != reqwest::StatusCode::OK {
            return Err(RoutineBridgeTransportError::Unknown(
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
        let reply = RoutineBridgeEnvelope::<R>::decode(&body)
            .map_err(|_| RoutineBridgeTransportError::Unknown(AfterSend::InvalidResponse))?;
        reply
            .validate_reply_to(request)
            .map_err(|_| RoutineBridgeTransportError::Unknown(AfterSend::MismatchedResponse))?;
        drop(response);
        if Instant::now() >= deadline {
            return Err(RoutineBridgeTransportError::Unknown(AfterSend::Deadline));
        }
        Ok(reply)
    }
}

fn before_remaining(deadline: Instant) -> RoutineBridgeTransportResult<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or(RoutineBridgeTransportError::NotSent(BeforeSend::Deadline))
}

fn read_bounded<R: Read>(
    reader: &mut R,
    length: Option<u64>,
    deadline: Instant,
) -> RoutineBridgeTransportResult<Vec<u8>> {
    if length.is_some_and(|length| length > ROUTINE_BRIDGE_BODY_LIMIT as u64) {
        return Err(RoutineBridgeTransportError::Unknown(
            AfterSend::ContextTooLarge,
        ));
    }
    let limit = ROUTINE_BRIDGE_BODY_LIMIT + 1;
    let capacity = length
        .map(|length| length as usize)
        .unwrap_or(8192)
        .min(limit);
    let mut body = Vec::with_capacity(capacity);
    let mut chunk = [0u8; 8192];
    loop {
        if Instant::now() >= deadline {
            return Err(RoutineBridgeTransportError::Unknown(AfterSend::Deadline));
        }
        let want = chunk.len().min(limit - body.len());
        let read = reader.read(&mut chunk[..want]).map_err(|error| {
            let timeout = error.kind() == std::io::ErrorKind::TimedOut
                || error
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<reqwest::Error>())
                    .is_some_and(reqwest::Error::is_timeout)
                || Instant::now() >= deadline;
            RoutineBridgeTransportError::Unknown(if timeout {
                AfterSend::Deadline
            } else {
                AfterSend::Transport
            })
        })?;
        if read == 0 {
            break;
        }
        if body.len() + read > body.capacity() {
            body.reserve_exact(limit - body.len());
        }
        body.extend_from_slice(&chunk[..read]);
        if body.len() > ROUTINE_BRIDGE_BODY_LIMIT {
            return Err(RoutineBridgeTransportError::Unknown(
                AfterSend::ContextTooLarge,
            ));
        }
    }
    Ok(body)
}

#[cfg(test)]
mod tests;
