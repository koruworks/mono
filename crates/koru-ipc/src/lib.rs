// SPDX-License-Identifier: MPL-2.0

//! **A per-user local endpoint, and how to talk over one.**
//!
//! A named pipe on Windows, a Unix socket elsewhere, named from the OS user,
//! the channel, and **the service** — so the `koru-store-server` host binds the
//! retained `koru-store` service beside `koru-agent`, and neither endpoint can
//! be mistaken for the other.
//!
//! ```text
//! let endpoint = Endpoint::for_service("koru-agent", koru_channel::segment())?;
//! let listener = Listener::bind(endpoint)?;      // ...if you are the agent
//! let stream   = client::connect(&endpoint)?;    // ...if you are talking to it
//! ```
//!
//! ## What this crate knows, and what it deliberately does not
//!
//! It knows how to *be* an endpoint — where it is, who may reach it, how to
//! claim it, and how to frame a message over it. It does **not** know what a
//! Store is, what an Agent is, or **how to launch either**. Starting a process
//! is a decision about a product; naming a pipe is not. That separation is the
//! reason this crate exists rather than the agent reaching into
//! another service crate for pipe ACLs, or writing a second copy of them
//! ([ADR-105](../../../docs/decisions/README.md)).
//!
//! So there is no `Launcher` here, no `ensure_running`, no retry-while-it-binds.
//! Process launch and supervision live with the product host that owns them.
//!
//! ## Connecting **is** the liveness check
//!
//! You cannot connect to a pipe nobody is listening on, so liveness answers
//! itself and there is no file to go stale. Binding is the exclusion — a second
//! process asking for the same name is refused by the operating system rather
//! than by a lease this code has to keep in step. Both properties came from
//! the old Store rendezvous and are the reason it stopped writing `store.json`,
//! `store.secret` and `store.lock` ([ADR-84](../../../docs/decisions/README.md)).
//!
//! ## Two versions live at two layers, and they are not the same number
//!
//! [`frame`] owns the shape of a frame: `KORU`, a version byte, a length, a
//! JSON body. **It takes the version byte as a parameter** and holds no
//! constant of its own, because the protocol spoken over a frame belongs to
//! whoever is speaking. Gateway discovery and the agent's long-lived stream
//! carry protocols versioned separately over identical bytes, which is the
//! whole reason this layer declines to have an opinion.
//!
//! A frame version this crate cannot read is a transport failure and the
//! connection is closed; a *protocol* version the peer does not speak is a
//! refusal the peer sends, which is a different sentence and a different layer.

pub mod client;
pub mod debug;
pub mod diagnose;
pub mod endpoint;
pub mod errors;
pub mod frame;
pub mod listener;
#[cfg(feature = "tokio")]
pub mod listener_async;
mod peer;
#[cfg(test)]
mod test_support;

pub use client::{connect, connect_timed, probe, set_timeouts, split};
pub use endpoint::Endpoint;
pub use errors::IpcError;
pub use listener::Listener;
pub use peer::{peer_pid, same_user};

/// The connection itself.
///
/// Re-exported so that naming one does not mean depending on `interprocess`:
/// a caller that has to write the type of what [`connect`] handed it should not
/// acquire a second view of the transport to do so. `Stream::split` is on it,
/// which is what a long-lived protocol needs — a `RecvHalf` for a reader thread
/// and a `SendHalf` for a writer queue, both `Send`.
pub use interprocess::local_socket::Stream;

#[cfg(feature = "tokio")]
pub use interprocess::local_socket::tokio::Stream as AsyncStream;
#[cfg(feature = "tokio")]
pub use listener_async::AsyncListener;

/// The service token for the Koru Store's rendezvous.
///
/// Here, rather than in a service-specific crate, for one reason: two services
/// must never derive the same endpoint name, and the only way to be sure of
/// that is for the tokens to sit side by side where somebody adding a third can
/// see the other two. It is a **name**, not knowledge of what a store is — this
/// crate cannot start one, talk to one, or say what one holds.
///
/// Retained across the host rename so old and new bundles contend for one
/// authoritative Store root during the compatibility interval.
pub const STORE: &str = "koru-store";

/// The service token for the Koru Agent's endpoint. See [`STORE`].
pub const AGENT: &str = "koru-agent";

/// The supervised terminal-session host's private endpoint.
///
/// Kept beside every other service token so a new endpoint cannot silently
/// reuse an existing product name. The Agent supervises this service, while
/// Koru Terminal and the API Gateway connect to it directly.
pub const SESSION_HOST: &str = "koru-session-host";

/// The private events-journal root claim. Its own token rather than the
/// Store's, because the two roots are siblings and either may be opened
/// without the other — a standalone events host on a live gateway's root
/// must fail to claim, and a Store repair must not contend with a journal.
pub const EVENTS: &str = "koru-events";

/// The public API gateway rendezvous. It is distinct from both Agent control
/// and the private Store root claim so their credentials cannot be confused.
pub const API_GATEWAY: &str = "koru-api-gateway";
/// A shell's short-lived update-reopen claim. The durable opaque token and
/// installation root further qualify this service through
/// [`Endpoint::for_service_claim`]. Holding the endpoint is process-level
/// idempotency; no application messages cross it.
///
/// The digest-qualified form was once 106 bytes against `sun_path`'s 104 on
/// macOS — see [`AGENT_GATEWAY_BRIDGE`] below for the trap. [ADR-417]'s move
/// of the runtime dir to `/tmp` puts the worst composed name at 70 bytes, and
/// `every_endpoint_this_crate_can_name_fits_in_sun_path` now owns the budget
/// ([TECHDEBT 91], fixed).
///
/// [ADR-417]: ../../../docs/decisions/ADR-417-sunpath.md
/// [TECHDEBT 91]: ../../../docs/contributing/techdebt/README.md
pub const REOPEN_CLAIM: &str = "koru-reopen";
/// The private Agent↔gateway byte bridge. The token is deliberately short:
/// it becomes a directory in a Unix socket path, and on macOS `sun_path`
/// holds 104 bytes against Linux's 108 — under the `/var/folders/…` user
/// temp dir, "koru-agent-gateway-bridge" pushed a digest-suffixed test
/// endpoint to 112 bytes and every bind failed. Nothing persists this name;
/// both sides derive the endpoint from this constant at each launch.
///
/// **Shortening this constant fixed one endpoint and not the rule.** Every
/// service added since has re-broken the limit, because nothing composes the
/// longest name this crate can produce and checks it. That test is what
/// [TECHDEBT 91](../../../docs/contributing/techdebt/README.md) actually asks for; the
/// relocation it also proposes is only what makes the test passable.
pub const AGENT_GATEWAY_BRIDGE: &str = "koru-bridge";

/// The voice worker's endpoint. Transcripts and a coarse level bucket cross
/// it; **audio never does**, because the worker opens the microphone itself
/// (ADR-273) — which is also why `frame::MAX_BODY` is never a question here,
/// the largest message this service carries being a sentence.
///
/// Here rather than in `apps/koru-voice` for the reason [`STORE`] gives: two
/// services must never derive the same endpoint name, and the only way to be
/// sure of that is for the tokens to sit side by side where somebody adding
/// one more can see the rest — and, since [`SERVICES`], where the length
/// budget can see it too.
pub const VOICE: &str = "koru-voice";

/// The delegate worker's private endpoint: `koru-agent.exe --worker delegate`
/// answers the gateway's `/v1/runs` routes over it (D12), and nothing else
/// dials it. Root-qualified like [`SESSION_HOST`], so two rooted stacks never
/// meet. Beside the other nine for the reason [`STORE`] gives.
pub const DELEGATE: &str = "koru-delegate";

/// Every service token this crate can name, as one table.
///
/// **It exists so the length budget can be tested rather than remembered.** A
/// Unix socket path lives in `sun_path`, which is 104 bytes on macOS, and the
/// composed name is `<runtime dir>/<service>/<segment>[.<digest>].sock` — so
/// adding a service is adding a path, and a path that does not fit fails at
/// `bind` on one platform and nowhere else. `every_endpoint_this_crate_can_name_fits_in_sun_path`
/// walks this table against every channel segment; a service missing from it is
/// a service nothing checks, which is how `koru-session-host` reached 114 bytes
/// unnoticed ([ADR-417], [TECHDEBT 91]).
///
/// [ADR-417]: ../../../docs/decisions/ADR-417-sunpath.md
/// [TECHDEBT 91]: ../../../docs/contributing/techdebt/README.md
pub const SERVICES: &[&str] = &[
    STORE,
    AGENT,
    SESSION_HOST,
    EVENTS,
    API_GATEWAY,
    REOPEN_CLAIM,
    AGENT_GATEWAY_BRIDGE,
    VOICE,
    DELEGATE,
];
