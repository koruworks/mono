// SPDX-License-Identifier: MPL-2.0

//! Asking the Agent for a first-party grant, and asking it about one.
//!
//! **Both halves of one short conversation, in the crate that already owns the
//! only reusable Agent handshake.** A first-party window calls [`request`]
//! before it connects to the Gateway and presents what comes back at admission;
//! the Gateway calls [`validate`] to find out whether the Agent minted it. Two
//! callers, opposite directions, the same five frames — which is why they are
//! one module rather than two copies of a handshake.
//!
//! It lives here rather than in `koru-agent-wire` for the reason that crate's
//! manifest already gives: the handshake needs `koru-ipc`, and taking it would
//! stop the wire being the serde-only leaf both sides depend on. The Agent's
//! own `client.rs` says the same thing about the same duplication.
//!
//! Nothing here starts an Agent. A grant is meaningful only from a *running*
//! Agent of the caller's own installation, so an Agent that is not there is an
//! answer — no grant — rather than something to fix by spawning one.

use std::io::Write as _;
use std::time::Duration;

use koru_agent_wire::{
    AgentGreeting, FRAME_VERSION, FirstPartyGrant, FromAgent, FromShell, PROTOCOL_VERSION, Request,
    Response, ShellHello,
};

/// Long enough for the Agent to answer off its event loop, short enough that a
/// window's startup never waits on a wedged supervisor. Admission fails closed
/// if this elapses, which is the safe direction: a destructive scope is not
/// granted because a check timed out.
const DEADLINE: Duration = Duration::from_secs(5);

/// What went wrong, at the granularity a caller can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantError {
    /// No Agent is listening on this channel's endpoint.
    NotRunning,
    /// It is listening and says this caller is not one of its windows.
    Refused(String),
    /// The conversation itself failed — framing, version, or the socket.
    Unavailable(String),
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning => formatter.write_str("the Koru Agent is not running"),
            Self::Refused(message) | Self::Unavailable(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for GrantError {}

/// The Agent's endpoint for a channel, rooted when the caller is.
///
/// A rooted product must reach its own rooted Agent, or a
/// rehearsal would be asking somebody's real installation to vouch for it.
pub fn agent_endpoint(
    segment: &str,
    root: Option<&std::path::Path>,
) -> Result<koru_ipc::Endpoint, GrantError> {
    let endpoint = match root {
        Some(root) => koru_ipc::Endpoint::for_service_channel_root(koru_ipc::AGENT, segment, root),
        None => koru_ipc::Endpoint::for_service(koru_ipc::AGENT, segment),
    };
    endpoint.map_err(|error| GrantError::Unavailable(error.to_string()))
}

/// **Is a Koru Agent listening for this channel right now?**
///
/// The discovery answer, not a remembered one. A process that dialled the Agent
/// once at start-up knows only whether *its own* link came up then — which is a
/// different question, and a worse one to decide anything by: an Agent started a
/// second after the window, or one that dropped and came back, leaves that
/// answer permanently wrong. This asks the endpoint, which is how every other
/// part of Koru finds the Agent.
///
/// No handshake, by the same rule [`koru_ipc::probe`] states: something is
/// listening, or nothing is. Starting one is deliberately not on offer here —
/// asking whether a supervisor exists must not be a reason to create one.
pub fn agent_running(segment: &str, root: Option<&std::path::Path>) -> bool {
    agent_endpoint(segment, root).is_ok_and(|endpoint| koru_ipc::probe(&endpoint))
}

/// Ask the supervisor to vouch for this process.
pub fn request(endpoint: &koru_ipc::Endpoint) -> Result<FirstPartyGrant, GrantError> {
    match ask(endpoint, Request::RequestFirstPartyGrant)? {
        Response::FirstPartyGrant { grant } => Ok(grant),
        Response::Failed { message, .. } => Err(GrantError::Refused(message)),
        _ => Err(GrantError::Unavailable(
            "the Agent answered a grant request with something else".into(),
        )),
    }
}

/// Spend a presented grant against the Agent that would have minted it.
///
/// **Fails closed.** Anything other than a live `true` — no Agent, a framing
/// mismatch, a timeout, an unexpected body — is `false`, because the caller is
/// deciding whether to hand out a destructive permission and "I could not
/// check" must never read as "yes".
pub fn validate(endpoint: &koru_ipc::Endpoint, grant: FirstPartyGrant) -> bool {
    matches!(
        ask(endpoint, Request::ValidateFirstPartyGrant { grant }),
        Ok(Response::FirstPartyGrantValidity { valid: true })
    )
}

fn ask(endpoint: &koru_ipc::Endpoint, body: Request) -> Result<Response, GrantError> {
    let (mut stream, _) = handshake(endpoint)?;
    ask_on_stream(&mut stream, body)
}

/// Read the running Agent's acknowledged identity without starting a service
/// or requesting a grant. Credential owners use this instead of reading or
/// minting a device identity themselves.
pub fn agent_welcome(
    endpoint: &koru_ipc::Endpoint,
) -> Result<koru_agent_wire::AgentWelcome, GrantError> {
    handshake(endpoint).map(|(_, welcome)| welcome)
}

fn handshake(
    endpoint: &koru_ipc::Endpoint,
) -> Result<(koru_ipc::Stream, koru_agent_wire::AgentWelcome), GrantError> {
    let mut stream = koru_ipc::connect(endpoint).map_err(|error| match error {
        koru_ipc::IpcError::NotRunning(_) => GrantError::NotRunning,
        other => GrantError::Unavailable(other.to_string()),
    })?;
    koru_ipc::set_timeouts(&stream, Some(DEADLINE));
    koru_ipc::frame::write_frame(
        &mut stream,
        FRAME_VERSION,
        &ShellHello {
            product: "koru-api-client".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .and_then(|()| stream.flush())
    .map_err(|error| GrantError::Unavailable(format!("Agent greeting failed: {error}")))?;
    let (version, greeting): (u8, AgentGreeting) = koru_ipc::frame::read_frame(&mut stream)
        .map_err(|error| GrantError::Unavailable(format!("Agent greeting failed: {error}")))?;
    if version != FRAME_VERSION {
        return Err(GrantError::Unavailable(
            "the Agent framing version is incompatible".into(),
        ));
    }
    match greeting {
        AgentGreeting::Welcome(welcome) => Ok((stream, *welcome)),
        _ => Err(GrantError::Unavailable(
            "the Agent protocol version is incompatible".into(),
        )),
    }
}

fn ask_on_stream(mut stream: &mut koru_ipc::Stream, body: Request) -> Result<Response, GrantError> {
    koru_ipc::frame::write_frame(
        &mut stream,
        FRAME_VERSION,
        &FromShell::Request { id: 1, body },
    )
    .and_then(|()| stream.flush())
    .map_err(|error| GrantError::Unavailable(format!("Agent grant request failed: {error}")))?;
    loop {
        let (version, answer): (u8, FromAgent) =
            koru_ipc::frame::read_frame(&mut stream).map_err(|error| {
                GrantError::Unavailable(format!("Agent grant response failed: {error}"))
            })?;
        if version != FRAME_VERSION {
            return Err(GrantError::Unavailable(
                "the Agent framing version changed".into(),
            ));
        }
        // A `Push` may arrive between the request and its reply — the module
        // note on `koru-agent-wire` says matching by arrival order is wrong the
        // first time somebody presses the hotkey mid-conversation — so this
        // waits for the id it sent.
        if let FromAgent::Response { id: 1, body } = answer {
            return Ok(body);
        }
    }
}
