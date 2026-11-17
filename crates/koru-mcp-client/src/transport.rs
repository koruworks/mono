// SPDX-License-Identifier: MPL-2.0

//! The two ports: how a request leaves, and where its bearer comes from.

use std::fmt;

use crate::Refusal;
use crate::sealed::Sealed;

/// Sends one request and returns one response.
///
/// **The adapter owns the address, the connection and the deadline.** The
/// client hands over headers and a body and reads back a status, two headers
/// and a body. It is a blocking call, as `koru_mcp_server::Backend::invoke`
/// is, because that is the seam a forwarding backend will sit behind.
///
/// **The deadline matters to the outcome.** The hosted server waits up to 15
/// seconds for Koru Cloud to acknowledge each write before it answers
/// `save_status_unknown` (`contracts/hosted-tools-v1.md` section 1), and up to
/// 30 seconds for a workspace to load (section 5, `workspace_loading`). An
/// adapter whose own deadline is shorter than the server's turns answers the
/// server would have given into [`TransportFailure::Unconfirmed`], which a
/// write reports as unknown.
pub trait Transport: Send + Sync {
    fn post(&self, request: &PostRequest) -> Result<PostResponse, TransportFailure>;
}

/// Why a request has no response.
///
/// The two are different facts about a write, so an adapter must not guess:
/// **when in doubt, [`Unconfirmed`](TransportFailure::Unconfirmed).**
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportFailure {
    /// Proven: no byte of the request left this process. The name did not
    /// resolve, the connection was refused before anything was written.
    NotSent,
    /// Anything else: a timeout, a reset, a response cut short. The server
    /// may have run the call.
    Unconfirmed,
}

/// Hands over the bearer for one request.
///
/// The credential's owner answers it: it holds the token, renews it and knows
/// when it cannot. An `Err` is the reason no request was sent, in the
/// client's own vocabulary ([`Refusal::TokenExpired`],
/// [`Refusal::ConnectionRevoked`], [`Refusal::SubscriptionEnded`]), and the
/// client returns it unchanged.
pub trait AccessTokens: Send + Sync {
    fn access_token(&self) -> Result<AccessToken, Refusal>;
}

/// A bearer token. It does not print, compare or serialize.
#[derive(Clone)]
pub struct AccessToken(String);

/// The text offered as a token cannot travel in a header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidAccessToken;

impl fmt::Display for InvalidAccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the access token is empty or holds a character a header cannot carry")
    }
}

impl std::error::Error for InvalidAccessToken {}

impl AccessToken {
    /// Accepts visible ASCII with no space, and nothing else, so a token can
    /// never add a second header to the request it rides in.
    pub fn new(token: impl Into<String>) -> Result<Self, InvalidAccessToken> {
        let token = token.into();
        if token.is_empty() || !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
            return Err(InvalidAccessToken);
        }
        Ok(Self(token))
    }

    /// The `Authorization` header's value.
    fn header(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(sealed)")
    }
}

/// One request, ready to send as a `POST`.
///
/// Built only by the client. An adapter copies [`PostRequest::headers`] and
/// [`PostRequest::body`] onto the wire and adds nothing of its own that the
/// server would compare with the body.
pub struct PostRequest {
    method: &'static str,
    /// The tool's name on `tools/call`.
    name: Option<String>,
    protocol_revision: &'static str,
    token: AccessToken,
    body: Sealed<Vec<u8>>,
}

impl PostRequest {
    pub(crate) fn new(
        method: &'static str,
        name: Option<String>,
        protocol_revision: &'static str,
        token: AccessToken,
        body: Vec<u8>,
    ) -> Self {
        Self {
            method,
            name,
            protocol_revision,
            token,
            body: Sealed::new(body),
        }
    }

    /// The MCP method this request carries, for an adapter's own bookkeeping.
    pub fn method(&self) -> &'static str {
        self.method
    }

    /// Every header the request needs, **the bearer among them**.
    ///
    /// The set is what the hosted server's front door checks (HOST.md,
    /// "Routes"): `MCP-Protocol-Version`, `Mcp-Method` and, on `tools/call`,
    /// `Mcp-Name` must each equal the body's own value, or the request is
    /// refused before it is read. `Accept` names both forms an answer to a
    /// `POST` may take.
    pub fn headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = vec![
            ("Content-Type", "application/json".to_owned()),
            ("Accept", "application/json, text/event-stream".to_owned()),
            ("MCP-Protocol-Version", self.protocol_revision.to_owned()),
            ("Mcp-Method", self.method.to_owned()),
        ];
        if let Some(name) = &self.name {
            headers.push(("Mcp-Name", name.clone()));
        }
        headers.push(("Authorization", self.token.header()));
        headers
    }

    /// The JSON-RPC message. It can hold a document's content.
    pub fn body(&self) -> &[u8] {
        self.body.expose()
    }
}

impl fmt::Debug for PostRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The method and the tool's name are vocabulary. The bearer and the
        // body are not printed.
        f.debug_struct("PostRequest")
            .field("method", &self.method)
            .field("name", &self.name)
            .field("protocol_revision", &self.protocol_revision)
            .field("body", &self.body)
            .finish_non_exhaustive()
    }
}

/// One response, as much of it as the client reads.
pub struct PostResponse {
    pub status: u16,
    /// The `Content-Type` header, when there was one.
    pub content_type: Option<String>,
    /// The `WWW-Authenticate` header, when there was one.
    pub www_authenticate: Option<String>,
    pub body: Vec<u8>,
}

impl fmt::Debug for PostResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The body can hold a document; the challenge header holds an
        // address. Neither is printed.
        f.debug_struct("PostResponse")
            .field("status", &self.status)
            .field("body_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}
