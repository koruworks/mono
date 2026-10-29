// SPDX-License-Identifier: MPL-2.0

//! **The Koru Agent — it owns the device.**
//!
//! Agent owns the device. Store owns the data. Shells own the experience.
//!
//! ```text
//! koru.exe ──┐
//! koru.exe ──┼─> koru-agent ──supervises──> koru-agent --worker gateway
//!            │      │                       koru-agent --worker sessions
//!            └──────┘  Open(target), Restore
//! ```
//!
//! Three things in Koru are machine-wide and used to live in whichever
//! `koru.exe` happened to start first — the global hotkey, the search overlay,
//! and the routing of a search result to the window that can show it — and a
//! fourth was missing entirely: **nothing started the store**. All four are
//! here.
//!
//! It owns **no workspace data**. Every Loro operation, snapshot, index and
//! durable write stays in the store; this process supervises, holds the
//! device's identity, and knows which window is showing what.
//!
//! ## One event loop; every other thread is a producer
//!
//! Exactly one thread may touch a window, and it is winit's. The hotkey has a
//! thread because `RegisterHotKey` delivers to the thread that registered; the
//! IPC listener has one because `accept` blocks; each connection has a reader
//! and a writer because a named pipe blocks when the peer stops reading. They
//! all raise an [`event::AgentEvent`] through winit's `EventLoopProxy`.
//!
//! **The loop performs no blocking IPC in either direction.** A shell that
//! hangs — or is being debugged, or is paging in after a sleep — must not
//! freeze the tray, the hotkey, the overlay or any other shell's routing.
//!
//! ## What is decided here and what is performed elsewhere
//!
//! [`registry`], [`route`], [`handshake`] and [`paths`] are pure: they answer
//! *which shell*, *deliver or launch*, *welcome or refuse*, *may I touch the
//! machine* — and none of them opens a socket, spawns a process or draws
//! anything. That is what makes every branch of them assertable without a
//! window, which is most of this crate's test suite.
//!
//! ## `--root` turns off machine-wide integration by default
//!
//! See [`paths`]. A gate runs several agents at once, and an agent that
//! registered the global hotkey during `cargo test` would take Ctrl+Shift+K off
//! whoever ran it. The explicit interactive `--root ... --tray` launch opts
//! back into global input so a developer can test it; Windows arbitrates chord
//! conflicts. Startup and URL registration remain unavailable to that root.

pub mod account;
pub mod agents_menu;
pub mod ai_chat;
pub mod app;
pub mod build_stamp;
pub mod cli;
pub mod cli_updates;
pub mod client;
pub mod cloud_host;
pub mod connection;
pub mod delegate_host;
pub mod delegate_report;
pub mod delegate_worker;
pub mod device;
pub mod dictation_coordinator;
pub mod enrollment;
pub mod event;
pub mod gateway_host;
pub mod gateway_worker;
pub mod global_input;
pub mod grants;
pub mod handshake;
pub mod hotkey;
pub mod identity;
pub mod network;
pub(crate) mod notification_activation;
pub mod paths;
pub mod registry;
pub mod reminders;
pub mod rendezvous;
pub mod route;
pub mod routing_log;
pub mod run;
pub mod service;
pub mod session_host;
pub mod session_worker;
pub mod tray;
pub mod update;
pub mod voice_window;
pub mod windows_identity;

pub use app::Agent;
pub use device::Device;
pub use event::{AgentEvent, Waker};
pub use paths::Paths;
pub use registry::{ConnectionId, Registry};
pub use route::{Routed, Router};
pub use run::{StartError, run};

/// This build's channel — the segment the agent binds and the store is found on.
///
/// Asked rather than written down: the agent, the shell and the store are built
/// by one cargo invocation, so a `--release` build moves all three to
/// `internal-release` together and a hardcoded string would move none of them.
pub fn segment() -> &'static str {
    koru_channel::segment()
}

/// Where the agent lives, given what the command line said.
///
/// `None` is the ordinary launch. An explicit root is passed through to the
/// store, redirects `device.json` and the desktop preferences, and refuses
/// machine-wide integration by default — see [`paths`].
pub fn paths_for(root: Option<std::path::PathBuf>) -> std::io::Result<Paths> {
    match root {
        Some(root) => Ok(Paths::for_root(root)),
        // **The channel root, not the store's.** They are siblings:
        // `Koru/<channel>/store/` is the store's and `Koru/<channel>/agent/` is
        // this one's. Putting the agent under the store's directory would make
        // a repair that clears it silently mean "become a different device".
        None => koru_channel::channel_root()
            .map(Paths::for_channel)
            .ok_or_else(|| {
                std::io::Error::other(
                    "could not work out where this machine keeps local application data",
                )
            }),
    }
}

/// Run the one-shot COM class factory Windows invokes for a notification
/// action. The manifest points at `koru-com-notification.exe`, not at the
/// long-lived Agent, so Windows can end this broker without ending scheduling,
/// tray ownership, or any other device service.
pub fn run_windows_notification_com_server() -> Result<(), String> {
    notification_activation::serve_once()
}

/// The agent's own endpoint.
///
/// `koru-agent` and `koru-api-gateway` are two **services** on one channel, so their
/// names differ and neither can be dialled for the other — the reason
/// `koru_ipc::Endpoint` is parameterized at all. They are one *image* now and
/// still two services: a worker role binds the service name its role owns, not
/// the one its executable is called.
///
/// Product services are rooted at the channel directory, not at its `store/`
/// child. Resolving the ordinary channel root therefore reaches the same name
/// as an unrooted client; a genuinely different `--root` remains isolated.
pub fn endpoint(paths: &Paths) -> Result<koru_ipc::Endpoint, koru_ipc::IpcError> {
    match paths.explicit_root() {
        Some(root) => {
            koru_ipc::Endpoint::for_service_channel_root(koru_ipc::AGENT, segment(), root)
        }
        None => koru_ipc::Endpoint::for_service(koru_ipc::AGENT, segment()),
    }
}

/// The private endpoint owned by the supervised terminal-session host.
///
/// The Agent derives and passes this root-qualified identity while supervising
/// the host; interactive traffic itself never transits the Agent.
pub fn session_endpoint(paths: &Paths) -> Result<koru_ipc::Endpoint, koru_ipc::IpcError> {
    match paths.explicit_root() {
        Some(root) => {
            koru_ipc::Endpoint::for_service_channel_root(koru_ipc::SESSION_HOST, segment(), root)
        }
        None => koru_ipc::Endpoint::for_service(koru_ipc::SESSION_HOST, segment()),
    }
}

/// The delegate worker's private endpoint, rooted the way the session
/// host's is — the same rule `koru_delegate_host::endpoint` states.
pub fn delegate_endpoint(paths: &Paths) -> Result<koru_ipc::Endpoint, koru_ipc::IpcError> {
    koru_delegate_host::endpoint(segment(), paths.explicit_root())
}

#[cfg(test)]
mod tests;
