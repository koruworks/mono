// SPDX-License-Identifier: MPL-2.0

//! **The Koru read worker.** Handed a read-only path to one file, it sends
//! back the file's words, page by page (TECHNICAL D4).
//!
//! ```text
//!   Store daemon (coordinator)                koru-read-worker
//!   supervises it through koru-agent-worker,  --root <store dir> --supervised
//!   in a Job Object with a memory ceiling     bootstrap line on stdin
//!            |                                         |
//!            +------ koru-ipc, koru_ipc::READ ---------+
//!              Status -> Status(WorkerStatus)    the handshake
//!              Read   -> Page ... Page, Done | Failed
//!              Cancel -> Failed(cancelled)
//! ```
//!
//! ## Why a process of its own
//!
//! It parses untrusted files with a library whose own README says timeouts
//! and running out of memory are still easy to trigger. A fault here must not
//! take the Store with it, and ending a process is the only reliable way to
//! give back what a hostile file made it hold. It is a **pure service**: no
//! Store session, no network call, no knowledge of workspaces, documents or
//! hashes — a path in, words out.
//!
//! ## What this build reads
//!
//! **Text only** (OCR roadmap Plan 1.3). A PDF's text layer, page by page,
//! accepted only where it looks like text ([`accept`]); `.txt`, `.md`, `.csv`
//! as they are and `.html` without its markup ([`plain`]). A page with no
//! usable text layer, or a picture, needs recognition, which this build does
//! not have: the job says so (`partial`, or `needs-reader`) and does nothing
//! more. Recognition arrives with Plan 2.3, behind the contract's `Reader`.
//!
//! ## The threads
//!
//! | | |
//! | --- | --- |
//! | main | reads the bootstrap, binds, then waits for a stop or the idle deadline |
//! | accept, and one per connection | frames in, events out ([`server`]) |
//! | one per job | runs [`job::run`] ([`service`]) |
//! | one per run of pages | interprets pages with `hayro`, abandoned at its budget ([`budget`]) |

pub mod accept;
pub mod bootstrap;
pub mod budget;
pub mod cli;
pub mod idle;
pub mod job;
pub mod log;
pub mod pdf;
pub mod plain;
pub mod server;
pub mod service;
pub mod text;

use std::sync::mpsc;
use std::time::Instant;

/// **The whole of `main`.** It returns rather than calling `process::exit`, so
/// the log guard drops and the last lines reach the file.
pub fn run() -> std::process::ExitCode {
    let invocation = match cli::parse(std::env::args()) {
        Ok(invocation) => invocation,
        Err(rejected) => rejected.report(),
    };
    match invocation.command {
        cli::Command::Help => {
            print!("{}", cli::help());
            return std::process::ExitCode::SUCCESS;
        }
        cli::Command::Version => {
            println!("{}", cli::version());
            return std::process::ExitCode::SUCCESS;
        }
        cli::Command::Serve => {}
    }

    // **The bootstrap line first.** The supervisor put this process in its
    // memory-limited job before writing it; nothing worth counting is
    // allocated until it has been read.
    let mut stdin = std::io::stdin();
    let bootstrap = if invocation.supervised {
        match bootstrap::read(&mut stdin) {
            Ok(bootstrap) => Some(bootstrap),
            Err(why) => {
                eprintln!("koru-read-worker: {why}");
                return std::process::ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    let Some(root) = invocation.root.clone().or_else(koru_channel::store_root) else {
        eprintln!("koru-read-worker: this build could not determine the channel's root");
        return std::process::ExitCode::FAILURE;
    };
    let _logging = log::init(&root, invocation.console);

    if let Some(bootstrap) = &bootstrap
        && let Err(why) = bootstrap::validate(bootstrap, koru_channel::segment())
    {
        tracing::error!("{why}");
        eprintln!("koru-read-worker: {why}");
        return std::process::ExitCode::FAILURE;
    }

    let (stop, stopped) = mpsc::channel::<&'static str>();
    // **The parent's death is the end of stdin.** A supervised worker whose
    // Store is gone has nobody to read for.
    if bootstrap.is_some() {
        let parent_gone = stop.clone();
        let watched =
            koru_agent_worker::ParentPipe::new(stdin).watch("koru-read-parent", move || {
                let _ = parent_gone.send("the Store that started it is gone");
            });
        if let Err(error) = watched {
            tracing::error!(%error, "the parent could not be watched");
            eprintln!("koru-read-worker: the parent could not be watched: {error}");
            return std::process::ExitCode::FAILURE;
        }
    }

    // **Bind before anything else that costs.** A second worker on this root
    // fails here, which makes "one worker per Store" the operating system's
    // fact rather than a race.
    let listener = match server::bind(&root) {
        Ok(listener) => listener,
        Err(koru_ipc::IpcError::AlreadyBound(_)) => {
            // Not a fault: whoever wanted a worker on this root has one.
            tracing::info!("another read worker already serves this root");
            eprintln!("koru-read-worker: another read worker already serves this root");
            return std::process::ExitCode::SUCCESS;
        }
        Err(error) => {
            tracing::error!(
                code = error.code(),
                "the read endpoint could not be claimed"
            );
            eprintln!("koru-read-worker: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let launch_id = bootstrap.map(|bootstrap| bootstrap.launch_id);
    let service = service::Service::new(
        job::Engine::product(budget::Abandoned::product()),
        launch_id,
    );
    let accepting = std::thread::Builder::new()
        .name("koru-read-accept".into())
        .spawn({
            let service = service.clone();
            move || server::serve(listener, service)
        });
    if let Err(error) = accepting {
        tracing::error!(%error, "the listening thread could not be started");
        return std::process::ExitCode::FAILURE;
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        supervised = invocation.supervised,
        "the read worker started"
    );

    let policy = idle::IdlePolicy::from_env();
    loop {
        match policy.decide(service.idle_for(Instant::now())) {
            idle::IdleAction::Exit => {
                tracing::info!("the read worker had nothing to do and is ending");
                break;
            }
            idle::IdleAction::Wait(wait) => match stopped.recv_timeout(wait) {
                Ok(why) => {
                    tracing::info!(why, "the read worker is ending");
                    break;
                }
                // The deadline: look again. (`stop` is held here, so the
                // channel never disconnects.)
                Err(_) => continue,
            },
        }
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod fixtures;
