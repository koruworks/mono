// SPDX-License-Identifier: MPL-2.0

//! The delegate worker's composition: `koru-agent.exe --worker delegate`.
//!
//! The Agent spawns this role after the sessions and gateway workers,
//! writes one [`LaunchBootstrap`] line on its stdin and keeps the pipe open
//! as the parent-death signal. [`run_supervised`] then composes the
//! orchestrator (`koru-delegate-runtime`) over the real ports:
//!
//! - `runs.db`, opened beside the store root (P1.3);
//! - the Run documents, through the gateway with `koru-api-client` — the
//!   seeded create for §7.1 step 3 and the sync socket for every commit
//!   after it — never a direct SQLite open (P1.0 §4.2);
//! - the session host over its private IPC at protocol v15 (P1.1, P2.3);
//! - the context capture, which commits and verifies each pin against the
//!   Store and lays the run's copies out under its working folder (P1.7,
//!   `context.rs`);
//! - the event bus, which turns each `RunFact` into the one `delegate.*`
//!   row it is and publishes it through the gateway (P1.8, `events.rs`);
//! - the run's own MCP endpoint and what it registered, both through the
//!   gateway (P1.11, `authority.rs`): the authority is issued before the
//!   provider starts, advanced for a second attempt, revoked when the run
//!   ends, and the versions the run registered are read back at each turn
//!   boundary;
//! - the preamble beside the executable (P1.12), the scope roots from the
//!   session host's projects (P1.5).
//!
//! It then answers the gateway's `/v1/runs` over the root-qualified
//! `koru-delegate` endpoint — one connection per call, the frames
//! `koru-delegate-api` defines — from an **actor thread that owns the
//! orchestrator**. Every run's session stream is pumped on a thread of its
//! own into that actor, and every API call is a message to it, so no run
//! I/O ever happens on a thread that answers a caller.

mod actor;
mod authority;
mod bootstrap;
mod context;
mod documents;
mod events;
mod ledger;
mod roots;
mod schedule_events;
mod scheduler;
pub mod schedules;
mod server;
mod sessions;
mod stubs;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use koru_agent_worker::ParentPipe;
use koru_delegate_runtime::{CountingCapture, Options, Orchestrator, Ports, ScheduleCoordinator};
use koru_delegate_sqlite::RunsDb;

pub use bootstrap::LaunchBootstrap;
pub use koru_delegate_api::ipc::Hello;

/// The log file beside `koru-agent.log`, `koru-api-gateway.log` and
/// `koru-session-host.log`, so one directory answers what happened.
pub const LOG_FILE: &str = "koru-delegate.log";

const LOG_FILTER: &str = "warn,koru=info,koru_delegate_host=info,koru_delegate_runtime=info";
const LOG_FILTER_DEBUG: &str =
    "warn,koru=debug,koru_delegate_host=debug,koru_delegate_runtime=debug";

/// How long the worker waits for the session host and the gateway, each
/// started before it by the same Agent, to answer.
const PEERS_DEADLINE: Duration = Duration::from_secs(90);

/// The role's private endpoint, root-qualified under an explicit root.
pub fn endpoint(
    segment: &str,
    explicit_root: Option<&Path>,
) -> Result<koru_ipc::Endpoint, koru_ipc::IpcError> {
    match explicit_root {
        Some(root) => {
            koru_ipc::Endpoint::for_service_channel_root(koru_ipc::DELEGATE, segment, root)
        }
        None => koru_ipc::Endpoint::for_service(koru_ipc::DELEGATE, segment),
    }
}

/// Run the delegate worker on this process until its parent goes or its
/// supervisor asks it to stop. Reads the bootstrap from stdin first.
pub fn run_supervised(explicit_root: Option<&Path>) -> Result<(), String> {
    let mut stdin = std::io::stdin();
    let bytes = koru_agent_worker::read_bootstrap_line(&mut stdin)
        .map_err(|error| format!("could not read the Agent bootstrap: {error}"))?;
    let bootstrap: LaunchBootstrap = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid Agent bootstrap: {error}"))?;
    drop(bytes);
    if bootstrap.explicit_root != explicit_root.is_some() {
        return Err("the bootstrap and the command line disagree about the root".into());
    }
    if bootstrap.segment != koru_channel::segment() {
        return Err("the bootstrap names another channel".into());
    }
    let debug = koru_env::switch(koru_env::name::KORU_DELEGATE_DEBUG);
    let _logging = koru_log::init_file(
        &bootstrap.store_directory,
        LOG_FILE,
        if debug { LOG_FILTER_DEBUG } else { LOG_FILTER },
    );
    tracing::info!(
        target: "koru::delegate",
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        launch = %bootstrap.launch_id,
        "delegate worker started"
    );

    let (stop_tx, stop_rx) = mpsc::channel::<&'static str>();
    let parent_stop = stop_tx.clone();
    ParentPipe::new(stdin)
        .watch("koru-delegate parent", move || {
            let _ = parent_stop.send("parent gone");
        })
        .map_err(|error| format!("could not watch the Agent bootstrap pipe: {error}"))?;

    serve(bootstrap, explicit_root, stop_tx, stop_rx, debug)
}

fn serve(
    bootstrap: LaunchBootstrap,
    explicit_root: Option<&Path>,
    stop_tx: mpsc::Sender<&'static str>,
    stop_rx: mpsc::Receiver<&'static str>,
    debug: bool,
) -> Result<(), String> {
    let deadline = Instant::now() + PEERS_DEADLINE;
    let session_endpoint = sessions::endpoint(&bootstrap.segment, explicit_root)
        .map_err(|error| format!("session endpoint: {error}"))?;
    let sessions =
        match sessions::HostSessions::connect(session_endpoint.clone(), deadline, &stop_rx) {
            Ok(sessions) => sessions,
            // The parent went while this worker was still waiting for its
            // peers: a clean stop, the way EOF is one once it serves.
            Err(Wait::Stopped) => return Ok(()),
            Err(Wait::Failed(why)) => return Err(why),
        };
    let schedule_sessions = sessions::HostSessions::for_scheduler(session_endpoint.clone())?;
    let gateway = match documents::connect_gateway(&bootstrap, explicit_root, deadline, &stop_rx) {
        Ok(gateway) => gateway,
        Err(Wait::Stopped) => return Ok(()),
        Err(Wait::Failed(why)) => return Err(why),
    };
    // One ledger connection behind a narrow mutex, shared by the scheduler
    // and the Orchestrator's run-side ledger port. Capture reads a run's
    // launch profile from its intent, and the bus reads the identity a run's
    // `created` fact first travelled under.
    let ledger = ledger::SharedLedger::new(
        RunsDb::open_beside(&bootstrap.store_directory)
            .map_err(|error| format!("runs.db: {error}"))?,
    );
    // The gateway builds a run's endpoint URL from the address it published,
    // so this worker overrides it only when a person named one.
    let mcp_base = koru_env::value(koru_env::name::KORU_DELEGATE_MCP_ENDPOINT);
    let device = bootstrap.device_id.clone();
    let device_id = koru_delegate_core::DeviceId::parse(&device)
        .unwrap_or_else(|| koru_delegate_core::DeviceId::from_uuid(uuid::Uuid::nil()));

    let settings = || {
        koru_settings::SettingsPaths::for_channel_root(
            &bootstrap.root,
            koru_channel::channel_root().as_deref(),
            &bootstrap.segment,
        )
    };
    let schedule_preparation = schedules::HostSchedulePreparation::new(
        schedule_sessions,
        roots::HostRoots::new(session_endpoint.clone(), settings()),
        context::ScheduleCapture::new(gateway.client.clone()),
    );
    let run_authorities = authority::AuthorityProjection::default();
    let ports = Ports {
        store: Box::new(ledger.clone()),
        documents: Box::new(documents::StoreDocuments::new(gateway.client.clone())),
        sessions: Box::new(sessions),
        // Wrapped so §7.2's captures are counted whichever implementation
        // is composed here (P1.17).
        capture: Box::new(CountingCapture::new(Box::new(context::HostCapture::new(
            gateway.client.clone(),
            ledger.clone(),
        )))),
        events: Box::new(events::BusRunEvents::new(
            gateway.client.clone(),
            ledger.clone(),
        )),
        preamble: Box::new(
            stubs::InstalledPreamble::new(installation_dir()).with_orientation_override(
                settings().map(|paths| paths.session_instructions_file()),
            ),
        ),
        // **Not `<delegate-root>/runs/` any more** (ADR-365). Where a run
        // that belongs to no project does its work is a person's choice, held
        // as a device-local setting and defaulting to this machine's
        // temporary directory; the rule for *which* settings a rooted stack
        // reads is `koru-settings`', so a rehearsal cannot read an
        // installation's.
        roots: Box::new(roots::HostRoots::new(session_endpoint, settings())),
        mcp: Box::new(authority::GatewayAuthority::with_projection(
            gateway.client.clone(),
            mcp_base,
            run_authorities.clone(),
        )),
        registrations: Box::new(authority::GatewayRegistrations::new(
            gateway.client.clone(),
            device_id,
        )),
        clock: Box::new(stubs::WallClock),
        sleep: Box::new(stubs::ThreadSleep),
    };
    let orchestrator = Orchestrator::new(
        ports,
        Options {
            debug,
            ..Options::default()
        },
    );
    let scheduler = ScheduleCoordinator::new(
        Box::new(schedules::StoreSchedules::new(gateway.client.clone())),
        Box::new(ledger.clone()),
        Box::new(schedule_preparation),
        Box::new(stubs::WallClock),
        device_id,
    )
    .with_events(Box::new(schedule_events::BusScheduleEvents::new(
        gateway.client.clone(),
        ledger.clone(),
    )));
    let schedule_event_authority = Arc::new(schedule_events::ScheduleEventAuthorityGate::new());
    let schedule_publisher = Box::new(schedule_events::BusScheduleEvents::publisher(
        gateway.client.clone(),
        ledger.clone(),
        Arc::clone(&schedule_event_authority),
    ));

    let (commands, inbox) = mpsc::channel();
    let mut actor = actor::Actor::new(orchestrator, commands.clone(), device.clone());
    let scheduler = scheduler::Handle::spawn(
        scheduler,
        gateway.client.clone(),
        commands.clone(),
        schedule_publisher,
        schedule_event_authority,
    )?;
    actor.attach_scheduler(scheduler.control());
    let listener = koru_ipc::Listener::bind(
        endpoint(&bootstrap.segment, explicit_root)
            .map_err(|error| format!("delegate endpoint: {error}"))?,
    )
    .map_err(|error| format!("could not bind the delegate endpoint: {error}"))?;
    server::spawn(
        listener,
        server::Identity {
            launch_id: bootstrap.launch_id.clone(),
            authority: bootstrap.authority().to_owned(),
            device_id: device.clone(),
            run_authority: bootstrap.run_authority().cloned(),
            run_authorities,
        },
        commands,
        stop_tx,
        actor.active_runs(),
        scheduler.control(),
    )?;
    tracing::info!(
        target: "koru::delegate",
        "delegate worker serving"
    );

    actor.restart();
    actor.run(inbox, stop_rx);
    scheduler.stop();
    tracing::info!(target: "koru::delegate", "delegate worker stopping");
    Ok(())
}

/// Why a wait for a peer ended without the peer.
pub(crate) enum Wait {
    /// The parent went, or the supervisor asked: not a failure.
    Stopped,
    Failed(String),
}

/// The installation's data directory: the executables' own on Windows,
/// the bundle's `Resources/` on macOS — where `docs/prompts/` is shipped.
fn installation_dir() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    #[cfg(target_os = "macos")]
    {
        if let Some(contents) = exe_dir.parent()
            && contents.join("Resources").is_dir()
        {
            return contents.join("Resources");
        }
    }
    exe_dir
}

#[cfg(test)]
mod tests;
