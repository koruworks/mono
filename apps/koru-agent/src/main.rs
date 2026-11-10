// SPDX-License-Identifier: MPL-2.0

// A release build is a GUI app: launched from Explorer, the taskbar or the Run
// key it must not drag a console along. Debug keeps the console -- it is where
// panics and every eprintln go -- and a test binary keeps it too, or
// `cargo test --release` would print into the void.
#![cfg_attr(all(not(debug_assertions), not(test)), windows_subsystem = "windows")]

//! The binary. Everything it does is in `koru-agent`'s library — see `lib.rs`.

use koru_agent::cli;

/// `info` for us, `warn` for everything else — the store's rule, and for the
/// same reason: without the second half a busy socket's `TRACE` drowns the few
/// lines that are about the agent. `RUST_LOG` overrides it.
///
/// `koru=info` is what carries the `koru::`-targeted lines the shared window
/// and GPU code emit; those are the ones that say whether the overlay was warm.
// noq-proto 1.1 warns when its idle-path timer asks a single-path connection
// to close a path: MultipathNotNegotiated/LastOpenPath mean the request was
// refused and the usable path stayed open. Keep its errors, while leaving Koru
// transport warnings visible.
// iroh's pkarr publisher warns on every backoff retry while the machine has
// no route — activity, not news; the mobile host demotes the same module.
const LOG_FILTER: &str = "warn,noq_proto::connection=error,iroh::address_lookup::pkarr=error,koru=info,koru_agent=info,koru_replication=info";
const LOG_FILTER_SYNC_DEBUG: &str = "warn,noq_proto::connection=error,iroh::address_lookup::pkarr=error,koru=info,koru_agent=info,koru_replication=info,koru_sync::peer=debug";

fn main() {
    // **Before anything else** — logging, CLI parsing, any single-instance
    // claim: a `--worker <role>` invocation is that role for its whole life
    // and must never half-become an Agent. Returns only when no worker
    // argument is present.
    koru_agent_worker::dispatch(
        std::env::args(),
        &[
            koru_agent::gateway_worker::role(),
            koru_agent::session_worker::role(),
            koru_agent::delegate_worker::role(),
        ],
    );

    let mut invocation = match cli::parse(std::env::args()) {
        Ok(i) => i,
        Err(rejected) => rejected.report(),
    };

    // **Resolved once, before the match, because all three verbs need it.**
    // `status` and `stop` have to derive the same endpoint name a running agent
    // bound — which follows `--root` — or they would report on the machine's
    // shared agent while the person was asking about theirs.
    //
    // Help and version are answered first so that neither can fail on a
    // filesystem question they have no interest in.
    if let cli::Command::Help = invocation.command {
        print!("{}", cli::help());
        return;
    }
    if let cli::Command::Version = invocation.command {
        println!("{}", cli::version());
        return;
    }
    if (invocation.open_profile.is_some() || invocation.inherit_stdio)
        && invocation.command != cli::Command::Open
    {
        fatal("--profile and --inherit-stdio are valid only with the open command");
    }
    if let Some(manifest) = invocation.rehearse_update.as_mut() {
        if invocation.command != cli::Command::Run {
            fatal("--rehearse-update is valid only when running the Agent");
        }
        let Some(root) = invocation.root.as_mut() else {
            fatal("--rehearse-update requires an explicit --root");
        };
        *root = match std::fs::canonicalize(&*root) {
            Ok(root) => root,
            Err(error) => fatal(&format!("the rehearsal root does not exist: {error}")),
        };
        *manifest = match std::fs::canonicalize(&*manifest) {
            Ok(manifest) if manifest.is_file() => manifest,
            Ok(_) => fatal("the rehearsal manifest is not a file"),
            Err(error) => fatal(&format!("the rehearsal manifest does not exist: {error}")),
        };
        let install = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            .and_then(|path| std::fs::canonicalize(path).ok());
        if install
            .as_ref()
            .is_none_or(|install| install == root || !install.starts_with(&*root))
        {
            fatal("a rehearsal Agent must run from an installation beneath its --root");
        }
    }
    let paths = match koru_agent::paths_for(invocation.root) {
        Ok(paths) => paths,
        Err(e) => fatal(&format!("{e}")),
    };

    if paths.owns_the_machine()
        && matches!(
            invocation.command,
            cli::Command::Run | cli::Command::OpenUrl(_) | cli::Command::NotificationActivation(_)
        )
    {
        let install_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));
        if let Some(install_dir) = install_dir {
            match koru_agent::windows_identity::ensure_registered(&install_dir) {
                Ok(koru_agent::windows_identity::Registration::Changed) => {
                    let mut restart = std::process::Command::new(
                        std::env::current_exe()
                            .unwrap_or_else(|_| install_dir.join("koru-agent.exe")),
                    );
                    restart.args(std::env::args_os().skip(1));
                    if let Err(error) = restart.spawn() {
                        fatal(&format!(
                            "Windows notification identity registered but Agent restart failed: {error}"
                        ));
                    }
                    return;
                }
                Ok(_) => {}
                Err(error) => eprintln!("koru-agent: {error}; native reminders remain unavailable"),
            }
        }
    }

    match invocation.command {
        cli::Command::Help | cli::Command::Version => unreachable!("answered above"),
        cli::Command::Run => {
            // An explicit root normally takes nothing machine-wide. The
            // interactive launcher opts into the tray and global input so it
            // has a visible identity and can exercise the real Voice gesture.
            // Start-at-login and URL registration remain refused.
            // Console logging is independent: a development stack needs both
            // its live sync trace and the product's normal tray controls.
            let show_tray = paths.owns_the_machine() || invocation.tray;
            // **Before anything that could fail**, and held for the whole
            // command: dropping the guard stops the writer, silently. Only for
            // `run` — `status` and `stop` answer a question and leave, and
            // creating a log directory as a side effect of one would be a
            // surprise. Beside the store's log rather than under `agent/`, so
            // the three processes' accounts of the same minute are read
            // together. The subscriber also brings the shared panic hook,
            // which this process did not have either.
            let log_filter = if koru_env::switch(koru_env::name::KORU_SYNC_DEBUG) {
                LOG_FILTER_SYNC_DEBUG
            } else {
                LOG_FILTER
            };
            let _logging = if invocation.console {
                koru_log::init_file_and_stdout(&paths.store_dir(), "koru-agent.log", log_filter)
            } else {
                koru_log::init_file(&paths.store_dir(), "koru-agent.log", log_filter)
            };
            tracing::info!(
                target: "koru::startup",
                version = env!("CARGO_PKG_VERSION"),
                pid = std::process::id(),
                root = %paths.root().display(),
                tray = show_tray,
                "agent started"
            );
            // The token travels no further than `run`, which writes the health
            // file once the endpoint is answering. **`status` and `stop` never
            // get it**, and would have no business with it: neither of them is
            // the process the updater restarted.
            match koru_agent::run(
                paths,
                show_tray,
                invocation.console,
                invocation.update_health_token,
                invocation.rehearse_update,
                None,
                None,
            ) {
                Ok(()) => tracing::info!(
                    target: "koru::startup",
                    pid = std::process::id(),
                    "agent stopped cleanly"
                ),
                // **Not a failure.** Somebody wanted an agent and there is one;
                // a second launch — from the Run key, or from a shell that
                // raced another — stands down quietly. Exiting 0 matters: a
                // shell spawning this and waiting for the endpoint should carry
                // on, because the endpoint is there.
                Err(koru_agent::StartError::AlreadyRunning) => {
                    eprintln!("koru-agent: an agent is already running for this channel");
                }
                Err(e) => fatal(&e.to_string()),
            }
        }
        // **What the registry command line invokes.** A person clicked a link
        // in a browser; from here to a window there is one router, and this is
        // its front door.
        cli::Command::OpenUrl(url) => {
            // Parsed before anything is dialled or started. A URL is
            // attacker-controlled and refusing it loudly is the whole contract
            // -- an unknown form never becomes "open something roughly like it".
            let link = match koru_link::parse(&url) {
                Ok(link) => link,
                Err(why) => fatal(&format!("{url}: {}", describe(why))),
            };
            if matches!(
                &link,
                koru_link::KoruLink::Skill { .. } | koru_link::KoruLink::Command { .. }
            ) {
                fatal("an agent skill or command link is only meaningful in a chat");
            }
            match koru_agent::client::open_link(&paths, link.clone()) {
                // Handed over. This process is done, and the agent it handed to
                // is the one with the windows.
                Ok(true) => {}
                // **Nobody was listening, so become the agent** rather than
                // spawning a second one and racing it for the endpoint. The OS
                // launched this process with a desktop-interactive token, which
                // is exactly what an agent needs.
                Ok(false) => {
                    match koru_agent::run(
                        paths.clone(),
                        true,
                        false,
                        None,
                        None,
                        Some(link.clone()),
                        None,
                    ) {
                        Ok(()) => {}
                        // Lost the bind to an agent that started in between. It is
                        // there now, so hand over and stand down -- the same
                        // not-a-failure a second `run` is.
                        Err(koru_agent::StartError::AlreadyRunning) => {
                            if let Err(why) = koru_agent::client::open_link(&paths, link) {
                                fatal(&why);
                            }
                        }
                        Err(e) => fatal(&e.to_string()),
                    }
                }
                Err(why) => fatal(&why),
            }
        }
        cli::Command::NotificationActivation(envelope) => {
            let activation = match koru_native_notifications::Activation::decode_opaque(&envelope) {
                Ok(activation) => activation,
                Err(error) => fatal(&error.to_string()),
            };
            match forward_notification_activation(&paths, &activation) {
                Ok(true) => {}
                Ok(false) => match koru_agent::run(
                    paths.clone(),
                    true,
                    false,
                    None,
                    None,
                    None,
                    Some(activation.clone()),
                ) {
                    Ok(()) => {}
                    Err(koru_agent::StartError::AlreadyRunning) => {
                        if let Err(error) = forward_notification_activation(&paths, &activation) {
                            fatal(&error);
                        }
                    }
                    Err(error) => fatal(&error.to_string()),
                },
                Err(error) => fatal(&error),
            }
        }
        // **Registration is this process's own business**, not a running
        // agent's: it is a per-user registry write about *this* executable, and
        // asking an agent that may not exist to do it would be a round trip to
        // reach the same syscall.
        cli::Command::RegisterUrlScheme | cli::Command::UnregisterUrlScheme => {
            if !paths.owns_the_machine() {
                fatal("an agent with an explicit --root does not register koru:// links");
            }
            let exe = match std::env::current_exe() {
                Ok(exe) => exe,
                Err(e) => fatal(&format!("this executable has no path: {e}")),
            };
            let register = invocation.command == cli::Command::RegisterUrlScheme;
            let outcome = if register {
                koru_url_scheme::register(&exe)
            } else {
                koru_url_scheme::unregister(&exe)
            };
            match outcome {
                Ok(()) if register => println!("koru:// links now open in {}", exe.display()),
                Ok(()) => println!("koru:// links are no longer handled by this build"),
                Err(e) => fatal(&e.to_string()),
            }
        }
        // **Clients of a running agent**, and none starts
        // one: "not running" is a legitimate answer to `status`, and a question
        // that had to start an agent to answer it would be changing the thing
        // it was asked about.
        cli::Command::Status => {
            if let Err(why) = koru_agent::client::status(&paths) {
                fatal(&why);
            }
        }
        // **A client of the ledger, not of the worker.** It starts
        // nothing and asks nothing: the numbers it prints are on the
        // disk, which is what makes them readable when the worker is
        // the thing that has gone wrong.
        cli::Command::Delegate(target) => {
            if let Err(why) =
                koru_agent::delegate_report::report_to_console(&paths, target.as_deref())
            {
                fatal(&why);
            }
        }
        cli::Command::Open => {
            if let Err(why) = koru_agent::client::open_workspace(
                &paths,
                invocation.open_profile.as_deref(),
                invocation.inherit_stdio,
            ) {
                fatal(&why);
            }
        }
        cli::Command::Pair => {
            if let Err(why) = koru_agent::client::pair(&paths) {
                fatal(&why);
            }
        }
        cli::Command::PairCancel => {
            if let Err(why) = koru_agent::client::cancel_pairing(&paths) {
                fatal(&why);
            }
        }
        cli::Command::RehearseDownload | cli::Command::RehearseInstall => {
            if paths.explicit_root().is_none() {
                fatal("rehearsal consent requires an explicit --root");
            }
            let choice = if invocation.command == cli::Command::RehearseDownload {
                koru_agent_wire::UpdateChoice::Download
            } else {
                koru_agent_wire::UpdateChoice::Install {
                    session_generation: 0,
                }
            };
            if let Err(why) = koru_agent::client::accept_rehearsal(&paths, choice) {
                fatal(&why);
            }
        }
        cli::Command::Stop => {
            if let Err(why) = koru_agent::client::stop(&paths) {
                fatal(&why);
            }
        }
        // **A client of the store, like Store stats** — not of a running
        // agent. The report a person needs while debugging is durable state,
        // so this mints its own session and starts the store if it is down.
        cli::Command::DebugStore(path) => match koru_agent::service::store_debug(&paths, &path) {
            Ok(value) => match serde_json::to_string_pretty(&value) {
                Ok(json) => println!("{json}"),
                Err(e) => fatal(&format!("could not print the report: {e}")),
            },
            Err(why) => fatal(&why),
        },
    }
}

fn forward_notification_activation(
    paths: &koru_agent::Paths,
    activation: &koru_native_notifications::Activation,
) -> Result<bool, String> {
    let mut delay = std::time::Duration::from_millis(10);
    for attempt in 0..8 {
        match koru_agent::client::notification_activation(paths, activation) {
            Ok(forwarded) => return Ok(forwarded),
            Err(koru_agent::client::ActivationForwardError::Other(error)) => return Err(error),
            Err(koru_agent::client::ActivationForwardError::Busy) if attempt < 7 => {
                std::thread::sleep(delay);
                delay = (delay * 2).min(std::time::Duration::from_millis(640));
            }
            Err(koru_agent::client::ActivationForwardError::Busy) => {
                return Err(
                    "notification activation remained busy after bounded retries; the gesture was not accepted"
                        .into(),
                );
            }
        }
    }
    unreachable!("the bounded retry loop always returns")
}

/// Why a link was refused, in words rather than in a variant name.
fn describe(why: koru_link::LinkError) -> &'static str {
    match why {
        koru_link::LinkError::NotKoru => "not a koru:// link",
        koru_link::LinkError::UnknownForm => "not a koru:// link this build understands",
        koru_link::LinkError::BadId => "a koru:// link whose id is not a uuid",
        koru_link::LinkError::BadName => "a koru:// agent link whose name is not valid",
        koru_link::LinkError::Illegal => "a koru:// link carrying characters that are not allowed",
    }
}

/// Say what went wrong, and stop.
///
/// **Exiting non-zero matters**: a shell spawns this binary and waits for its
/// endpoint, and a process that exits 0 having bound nothing is
/// indistinguishable from one that worked.
fn fatal(why: &str) -> ! {
    eprintln!("koru-agent: {why}");
    std::process::exit(1)
}
