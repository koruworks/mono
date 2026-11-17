// SPDX-License-Identifier: MPL-2.0

//! The Koru API Gateway runtime: public admission, domain workers, and bounded
//! private service bridges owned by the API gateway core.
//!
//! **This is a library with no program in it, and that is the point.** The one
//! host that drives it is `koru-agent.exe --worker gateway`
//! (`apps/koru-agent/src/gateway_worker.rs`): it parses arguments, resolves a
//! root, reads a bootstrap line from its own stdin, prints to stderr and
//! chooses an exit code. Everything below that line — open the workers, bind the
//! rendezvous, serve the router, shut down — lives here.
//!
//! There *was* a second host, `koru-api-gateway.exe`, with `src/main.rs` and a
//! `cli` module beside this one. Plan 7 of the Gateway worker roadmap deleted
//! it, together with the `status`, `stop` and `unfiled` commands that were its
//! only reason to parse a command line. The split survives it and is still
//! load-bearing, because it is what let the migration happen at all: the
//! runtime reads no arguments, guesses no root and calls no `process::exit`, so
//! a host could be replaced without touching the composition.
//!
//! - [`GatewayConfig`] carries what a host resolved *before* a runtime existed:
//!   the channel root, this image's build identity, and the supervisor's
//!   bootstrap. The runtime reads no arguments and guesses no root.
//! - [`start`] performs the whole composition in one documented order and hands
//!   back a [`Gateway`] whose [`Gateway::readiness`] is the record a supervisor
//!   may publish. [`Gateway::serve`] runs until shutdown and returns a
//!   [`Completion`]. [`Gateway::shutdown_request`] is how a host stops it
//!   without being a client — the seam a worker role's parent-death watcher
//!   needs, and the reason it carries no credential.
//!
//! ## The three orderings this crate exists to keep
//!
//! Each is stated where it happens, with the reason, because each is easy to
//! reorder by accident and none of them fails loudly when it is:
//!
//! 1. **[`load_dotenv`] runs before any thread or runtime exists.** It is not
//!    called by [`start`] at all — it cannot be, because setting a process
//!    variable races anything reading one. The host calls it first.
//! 2. **The root-qualified rendezvous binds before Store opens**, so two
//!    gateway images cannot race into two owners for one root. See [`start`].
//! 3. **The private bridges connect before the rendezvous binds**, so a child
//!    is never discoverable before the Agent's edge into it exists. See
//!    [`start`].
//!
//! ## What is deliberately not here
//!
//! `status`, `stop` and `unfiled` were one-shot host commands that never opened
//! a Store. They lived in the binary, the runtime never knew they existed, and
//! Plan 7 deleted all three with it (roadmap decision 5): they became no Agent
//! verb and were not preserved as developer commands. What replaced the one
//! anybody used is a *line* rather than a command — `koru-agent status` reports
//! whether a gateway is serving on this root, probed against this crate's own
//! rendezvous rather than asked of the supervisor.

mod bootstrap;
mod bot_ingress;
mod composition;
mod discovery;
mod lifecycle;
#[cfg(all(test, windows))]
mod routine_bridge;

pub use lifecycle::{
    BuildIdentity, Completion, Gateway, GatewayConfig, Readiness, ShutdownRequest, StoreCompletion,
    init_logging, start,
};

/// A `.env` in the working directory, for a developer's session.
///
/// **Ordering (1): call this before you build a runtime.** Setting a process
/// variable is a data race against anything reading one — that is the contract
/// `koru_env::load_file` documents — so this must run while the process is
/// still single-threaded. It is a free function on the crate root, and not a
/// step inside [`start`], precisely so that it *cannot* be reached from inside
/// an already-running `#[tokio::main]`: a host that calls [`start`] has already
/// missed the window.
///
/// It is not fatal: a gateway that will not start because of a stray line in a
/// convenience file is a worse outcome than one that starts without the
/// convenience. But an unknown key is *named* and refused rather than skipped,
/// so a typo cannot leave a sink silently off. The names are printed and the
/// values never are.
pub fn load_dotenv() {
    match koru_env::load_dotenv() {
        Ok(None) => {}
        Ok(Some(loaded)) => {
            if !loaded.names.is_empty() {
                eprintln!(
                    "koru-api-gateway: loaded {} from {}",
                    loaded.names.join(", "),
                    loaded.path.display()
                );
            }
        }
        Err(error) => eprintln!("koru-api-gateway: .env ignored — {error}"),
    }
}
