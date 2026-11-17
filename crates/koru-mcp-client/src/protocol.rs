// SPDX-License-Identifier: MPL-2.0

//! One JSON-RPC message out, one back.
//!
//! **Taken from HOST.md, not from either published contract, and not
//! observed.** `docs/cloud-platform/mcp/HOST.md` ("Routes") describes the
//! hosted server's front door as built:
//!
//! - MCP is `POST` only, one message per request, with no protocol session.
//! - A request of revision 2026-07-28 carries `MCP-Protocol-Version`,
//!   `Mcp-Method` and, on `tools/call`, `Mcp-Name`, each equal to the body's
//!   own value; the revision is also in the body's `_meta`. A mismatch is
//!   `400` with JSON-RPC `-32020`.
//! - A revision the server does not speak is `400` with `-32022`, listing the
//!   ones it does.
//! - A missing or invalid token is `401` with a `WWW-Authenticate` challenge.
//! - A Read connection calling an edit tool is `403 insufficient_scope`.
//! - A method the server does not implement is `404` with `-32601`.
//! - The order is: the body, then the token, then the headers. So each of
//!   these refusals is given before any tool runs, which is what lets a write
//!   refused by one be reported as not saved.
//!
//! The pinned `rmcp` (3.9.0) knows this revision (`ProtocolVersion::
//! V_2026_07_28`, its `LATEST`) and the two error codes
//! (`ErrorCode::UNSUPPORTED_PROTOCOL_VERSION`, `ErrorCode::HEADER_MISMATCH`).
//! This module does not link it: three methods and one result shape are
//! less than the library, and the fake answers with that library's own
//! types, so a disagreement between the two shows up as a failing test.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::outcome::{Refusal, ToolAnswer};
use crate::transport::{AccessToken, PostRequest, PostResponse};

/// The one MCP revision this client speaks.
///
/// The design this was built from says the hosted server accepts this
/// revision only; HOST.md says the built server also accepts three older
/// ones. Either way this is the newest both name, and the only one with no
/// handshake to keep state for.
pub const PROTOCOL_REVISION: &str = "2026-07-28";

pub(crate) const TOOLS_CALL: &str = "tools/call";
pub(crate) const TOOLS_LIST: &str = "tools/list";

/// The JSON-RPC code for a revision the server does not speak.
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
/// The JSON-RPC code for a header that differs from the body.
const HEADER_MISMATCH: i64 = -32020;
const METHOD_NOT_FOUND: i64 = -32601;

/// Request ids. One process-wide counter per client is enough: every request
/// is its own exchange, and the id only has to match its own answer.
pub(crate) struct Ids(AtomicU64);

impl Ids {
    pub(crate) fn new() -> Self {
        Self(AtomicU64::new(1))
    }

    pub(crate) fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

/// The `_meta` every request of this revision carries. The revision and the
/// client's capabilities are required by the revision's schema (as `rmcp`
/// 3.9.0 reads it, `model/meta.rs`, `RequestMetaObject`); the client's name is
/// optional and is this crate's, never a person's or a device's.
fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": PROTOCOL_REVISION,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {
            "name": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

fn request(
    id: u64,
    method: &'static str,
    name: Option<&str>,
    params: Value,
    token: AccessToken,
) -> PostRequest {
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    PostRequest::new(
        method,
        name.map(str::to_owned),
        PROTOCOL_REVISION,
        token,
        // A `Value` always serializes.
        serde_json::to_vec(&body).unwrap_or_default(),
    )
}

pub(crate) fn tools_call(
    id: u64,
    name: &str,
    arguments: &Value,
    token: AccessToken,
) -> PostRequest {
    request(
        id,
        TOOLS_CALL,
        Some(name),
        json!({ "name": name, "arguments": arguments, "_meta": meta() }),
        token,
    )
}

pub(crate) fn tools_list(id: u64, cursor: Option<&str>, token: AccessToken) -> PostRequest {
    let mut params = json!({ "_meta": meta() });
    if let Some(cursor) = cursor {
        params["cursor"] = json!(cursor);
    }
    request(id, TOOLS_LIST, None, params, token)
}

/// What a response is, before a method gives its result a meaning.
pub(crate) enum Reply {
    /// The JSON-RPC `result`.
    Result(Value),
    /// A refusal the front door or admission gives before anything runs.
    Refused(Refusal),
    /// Anything else.
    NotUnderstood,
}

/// Read one response to request `id`.
pub(crate) fn read_reply(id: u64, response: &PostResponse) -> Reply {
    let message = message_of(response);
    // Whatever the body says: the server did not accept the bearer.
    if response.status == 401 {
        return Reply::Refused(Refusal::TokenExpired);
    }
    // Admission's refusals keep their own meaning under any status.
    if let Some(refusal) = message
        .as_ref()
        .and_then(|message| message.get("error"))
        .and_then(admission_refusal)
    {
        return Reply::Refused(refusal);
    }
    if response.status == 403 {
        return Reply::Refused(if names_insufficient_scope(response, message.as_ref()) {
            Refusal::InsufficientScope
        } else {
            Refusal::Forbidden
        });
    }
    let Some(message) = message else {
        return Reply::NotUnderstood;
    };
    // An answer to another request is not an answer. An error may leave the
    // id out, when the server could not read the request's own.
    match message.get("id") {
        None | Some(Value::Null) if message.get("error").is_some() => {}
        Some(answered) if answered.as_u64() == Some(id) => {}
        _ => return Reply::NotUnderstood,
    }
    if let Some(error) = message.get("error") {
        return rpc_error(error);
    }
    match (response.status, message.get("result")) {
        (200, Some(result @ Value::Object(_))) => Reply::Result(result.clone()),
        _ => Reply::NotUnderstood,
    }
}

/// Admission's refusal, when a JSON-RPC error names one in its data.
///
/// **In no document.** A tool call answers these three as a tool error
/// (`contracts/hosted-tools-v1.md` section 2). Neither published document
/// says how they travel when the method is not a tool call, or when the
/// server refuses before it builds a tool result; a server that answers a
/// JSON-RPC error instead would have to name the same code somewhere, and
/// `data.code` is where this client looks.
fn admission_refusal(error: &Value) -> Option<Refusal> {
    match error.get("data")?.get("code")?.as_str()? {
        "authorization_revoked" => Some(Refusal::ConnectionRevoked),
        "cloud_subscription_ended" => Some(Refusal::SubscriptionEnded),
        "authorization_unavailable" => Some(Refusal::AuthorizationUnavailable),
        _ => None,
    }
}

/// A JSON-RPC error, when it is one that proves nothing ran.
fn rpc_error(error: &Value) -> Reply {
    match error.get("code").and_then(Value::as_i64) {
        Some(UNSUPPORTED_PROTOCOL_VERSION) => Reply::Refused(Refusal::RevisionRefused {
            supported: supported_revisions(error),
        }),
        Some(rpc_code @ (HEADER_MISMATCH | METHOD_NOT_FOUND)) => {
            Reply::Refused(Refusal::RequestRefused { rpc_code })
        }
        // Every other JSON-RPC error could have been raised after a tool
        // began. It proves nothing.
        _ => Reply::NotUnderstood,
    }
}

/// The revisions a `-32022` lists, kept only when each looks like a date, so
/// that nothing else the server wrote is carried in a printable field.
fn supported_revisions(error: &Value) -> Vec<String> {
    error
        .get("data")
        .and_then(|data| data.get("supported"))
        .and_then(Value::as_array)
        .map(|supported| {
            supported
                .iter()
                .filter_map(Value::as_str)
                .filter(|revision| {
                    revision.len() == 10
                        && revision
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || byte == b'-')
                })
                .map(str::to_owned)
                .take(16)
                .collect()
        })
        .unwrap_or_default()
}

fn names_insufficient_scope(response: &PostResponse, message: Option<&Value>) -> bool {
    const SCOPE: &str = "insufficient_scope";
    if response
        .www_authenticate
        .as_deref()
        .is_some_and(|challenge| challenge.contains(SCOPE))
    {
        return true;
    }
    let Some(message) = message else {
        return false;
    };
    // RFC 6750's own body, `{ "error": "insufficient_scope" }`, or a JSON-RPC
    // error naming the code in its data.
    message.get("error").and_then(Value::as_str) == Some(SCOPE)
        || message
            .get("error")
            .and_then(|error| error.get("data"))
            .and_then(|data| data.get("code"))
            .and_then(Value::as_str)
            == Some(SCOPE)
}

/// The JSON-RPC message in a response body, as plain JSON or as the first
/// message of an event stream. `Accept` offers both, as the revision requires,
/// so both are read; which one the hosted server sends has not been observed.
fn message_of(response: &PostResponse) -> Option<Value> {
    let event_stream = response.content_type.as_deref().is_some_and(|kind| {
        kind.trim_start()
            .to_ascii_lowercase()
            .starts_with("text/event-stream")
    });
    let message: Value = if event_stream {
        first_event(std::str::from_utf8(&response.body).ok()?)?
    } else {
        serde_json::from_slice(&response.body).ok()?
    };
    message.is_object().then_some(message)
}

/// The first event of a stream whose data is a JSON object.
fn first_event(stream: &str) -> Option<Value> {
    let mut data = String::new();
    // A final event may end the body without a blank line after it.
    for line in stream.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if let Ok(message @ Value::Object(_)) = serde_json::from_str::<Value>(&data) {
                return Some(message);
            }
            data.clear();
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    None
}

/// Give a `tools/call` reply its shape: a success or a failure, each with the
/// tool's object (`crates/koru-mcp-server/MCP.md`, "Results": a failure is
/// `{ code, message, details? }` under `isError`, and every answer's
/// structured content is an object).
pub(crate) fn tool_answer(reply: Reply) -> ToolAnswer {
    let result = match reply {
        Reply::Result(result) => result,
        Reply::Refused(refusal) => return ToolAnswer::Refused(refusal),
        Reply::NotUnderstood => return ToolAnswer::NotUnderstood,
    };
    // This revision's results say whether they are complete. One that is not
    // is asking for something this client cannot give.
    if result
        .get("resultType")
        .is_some_and(|kind| kind.as_str() != Some("complete"))
    {
        return ToolAnswer::NotUnderstood;
    }
    let failed = match result.get("isError") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(failed)) => *failed,
        Some(_) => return ToolAnswer::NotUnderstood,
    };
    match result.get("structuredContent") {
        Some(object @ Value::Object(_)) if failed => ToolAnswer::Failure(object.clone()),
        Some(object @ Value::Object(_)) => ToolAnswer::Success(object.clone()),
        _ => ToolAnswer::NotUnderstood,
    }
}

#[cfg(test)]
mod tests;
