// SPDX-License-Identifier: MPL-2.0

//! Supervised process boundary for terminal sessions.
//!
//! The listener runs on one current-thread Tokio runtime. PTY, parser, writer,
//! and waiter work remains inside `koru-session-runtime`; this host owns only
//! endpoint admission, request routing, bounded connection lanes, and the
//! summary-only registry.

mod lifecycle;
pub mod projects;
mod server;

use std::path::{Path, PathBuf};

pub use server::{HostError, serve};

/// The inherited Agent bootstrap pipe after its first framed line was read.
/// EOF is the cross-platform parent-death signal; monitoring it blocks without
/// polling and contains no session content.
///
/// **Owned by `koru-agent-worker`, beside the reader that leaves the pipe
/// open.** This host had a private copy of both, and the gateway role would
/// have been the third; the mechanism is a property of any self-hosted worker
/// and none of it is knowledge about sessions.
pub use koru_agent_worker::ParentPipe;

/// The whole supervised composition, callable by both launch paths: read
/// the one-line Agent bootstrap from stdin, build the current-thread
/// runtime, and serve until the host stops or the parent dies.
///
/// The `koru-session-host` binary's `run --supervised` calls this, and so
/// does `koru-agent.exe --worker sessions` — one entry, so the two paths
/// cannot drift while both exist.
pub fn run_supervised(explicit_root: Option<&Path>) -> Result<(), String> {
    let mut stdin = std::io::stdin();
    let bytes = koru_agent_worker::read_bootstrap_line(&mut stdin)
        .map_err(|error| format!("could not read Agent bootstrap: {error}"))?;
    let bootstrap: koru_session_wire::LaunchBootstrap = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid Agent bootstrap: {error}"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start session runtime: {error}"))?;
    let local = tokio::task::LocalSet::new();
    local
        .block_on(
            &runtime,
            serve(bootstrap, explicit_root, Some(ParentPipe::new(stdin))),
        )
        .map_err(|error| error.to_string())
}

pub fn endpoint(
    segment: &str,
    explicit_root: Option<&Path>,
) -> Result<koru_ipc::Endpoint, koru_ipc::IpcError> {
    match explicit_root {
        Some(root) => {
            koru_ipc::Endpoint::for_service_channel_root(koru_ipc::SESSION_HOST, segment, root)
        }
        None => koru_ipc::Endpoint::for_service(koru_ipc::SESSION_HOST, segment),
    }
}

pub fn resolved_root(explicit_root: Option<PathBuf>) -> Result<PathBuf, String> {
    match explicit_root {
        Some(root) => Ok(root),
        None => koru_channel::channel_root()
            .ok_or_else(|| "could not resolve the channel root".to_owned()),
    }
}

pub fn protocol_offer() -> koru_session_protocol::ProtocolOffer {
    use koru_session_protocol::{
        CapabilityName, CapabilitySet, MAX_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION, ProtocolRange,
        capability,
    };
    use std::str::FromStr;

    let capabilities = [
        capability::CONTROLLER_LEASES,
        capability::RENDER_DELTAS,
        capability::SCROLLBACK_PAGES,
        capability::RAW_WRITE,
        capability::TERMINAL_KEYS,
        capability::RESIZE,
        capability::INTERRUPT,
        capability::TERMINATE,
        capability::ATTACHMENTS,
        capability::QUOTED_PATH_DELIVERY,
        capability::PROFILE_LAUNCH,
        capability::OBSERVER_OUTPUT,
        capability::OBSERVATION_CURSORS,
        capability::MANAGED_SESSIONS,
        capability::MANAGED_EVENTS,
        capability::MANAGED_CANCEL,
        capability::MANAGED_APPROVAL,
        capability::DYNAMIC_APPROVALS,
        capability::PRESENTED_ATTENTION,
        capability::MANAGED_USAGE,
        capability::PROVIDER_READINESS,
        capability::PROVIDER_MODELS,
        capability::NATIVE_ATTACHMENT_DELIVERY,
        capability::PROJECTS,
        capability::WORKSPACE_LIFECYCLE,
        capability::DELEGATION,
        capability::SESSION_ORIGIN,
        capability::LAUNCH_SELECTION,
        capability::LAUNCH_RESUME,
    ]
    .into_iter()
    .map(|name| CapabilityName::from_str(name).expect("built-in capability"));
    koru_session_protocol::ProtocolOffer {
        versions: ProtocolRange {
            min: MIN_PROTOCOL_VERSION,
            max: MAX_PROTOCOL_VERSION,
        },
        capabilities: CapabilitySet::new(capabilities),
    }
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_ordinary_channel_root_meets_an_unrooted_session_client() {
        let Some(channel_root) = koru_channel::channel_root() else {
            return;
        };
        let unrooted = endpoint(koru_channel::segment(), None).unwrap();
        let resolved = endpoint(koru_channel::segment(), Some(&channel_root)).unwrap();
        assert_eq!(unrooted, resolved);
    }

    #[test]
    fn explicit_channel_roots_stay_isolated_and_derivable() {
        let isolated = tempfile::tempdir().unwrap();
        let isolated_endpoint = endpoint(koru_channel::segment(), Some(isolated.path())).unwrap();
        let expected = koru_ipc::Endpoint::for_service_channel_root(
            koru_ipc::SESSION_HOST,
            koru_channel::segment(),
            isolated.path(),
        )
        .unwrap();
        let ordinary = endpoint(koru_channel::segment(), None).unwrap();
        assert_eq!(isolated_endpoint, expected);
        assert_ne!(isolated_endpoint, ordinary);
    }
}
