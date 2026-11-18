// SPDX-License-Identifier: MPL-2.0

//! **A command line that is not understood is not run.**
//!
//! The same rule and the same shape as `apps/koru-store-server/src/cli.rs` and
//! `crates/koru-workspace-host/src/cli.rs`: one table that both the parser and the help
//! read, so a flag cannot be accepted without being documented or documented
//! without being accepted. An unknown argument names itself, suggests the
//! nearest real flag, and exits 2.
//!
//! Ignoring an unrecognised argument sounds harmless and is the same failure as
//! guessing a directory. Here it is worse than usual: a mistyped `--root` would
//! silently operate on the *channel's own* root, including its data and startup
//! registration. An interactive rooted `--tray` launch may deliberately opt
//! into global input, but it still must not touch the ordinary installation.
//!
//! **The strictness has teeth in the other direction too.** The updater restarts
//! this binary with `--update-health-token <token>`, so a flag missing from the
//! table below is not a cosmetic gap: the agent would exit 2 on its first
//! launch, the updater would see the process it was waiting on gone, and a
//! perfectly good install would roll back. Anything that will ever restart the
//! agent has to be represented here *before* it does.

use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    /// Run the agent. The default when no subcommand is given.
    Run,
    /// Report on a running agent: its device, its shells, what it is holding.
    Status,
    /// What the delegate worker is holding, read from its ledger rather
    /// than from the worker. With a run id, print that run.
    ///
    /// **Not a client of the delegate worker.** It reads `runs.db` and
    /// `artifacts.db` directly, the way `koru-store-server projection`
    /// reads `projection.db`, because a worker that will not answer is
    /// exactly when its ledger is worth looking at (P1.17).
    Delegate(Option<String>),
    /// Read the Bots engine ledger without starting or contacting its worker.
    Bots,
    /// Ask a running Agent to focus or launch a Koru workspace window.
    Open,
    /// Display a short-lived device-pairing QR from the running Agent.
    Pair,
    /// Cancel the current device-pairing invitation.
    PairCancel,
    /// Accept the download step of a local update rehearsal. This is a
    /// root-qualified release-gate control, not an installed-channel command.
    RehearseDownload,
    /// Accept the install step of a local update rehearsal. See
    /// [`Self::RehearseDownload`].
    RehearseInstall,
    /// Ask a running agent to stop.
    Stop,
    /// Print one of the store's diagnostic reports, e.g. `debug documents/<id>`.
    ///
    /// **A client of the store, not of a running agent** — like the Store
    /// stats flow, this mints its own session and starts the store if it is
    /// down, because the answer it gives is about durable state.
    DebugStore(String),
    /// Become the build that opens `koru://` links.
    RegisterUrlScheme,
    /// Stop opening them.
    UnregisterUrlScheme,
    /// Follow a `koru://` link.
    ///
    /// **What the registry command line invokes**, not something a person
    /// types. The URL is carried as a value rather than as a bare argument, so
    /// the strict parser still rejects everything it does not recognise --
    /// accepting an unknown first argument because it happened to look like a
    /// URL is exactly the guess `cli.rs` exists to refuse.
    OpenUrl(String),
    /// Forward a bounded content-free native notification activation. This is
    /// an OS bootstrap seam, not a user-facing navigation command.
    NotificationActivation(String),
    Help,
    Version,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Invocation {
    pub command: Command,
    /// Override the root. **Turns off machine-wide integration by default** —
    /// see [`crate::paths`]. Chiefly for the suite and for running a second
    /// agent deliberately.
    pub root: Option<PathBuf>,
    /// Stay in the console instead of going to the notification area.
    pub console: bool,
    /// Show the notification-area icon and enable global input for an
    /// explicitly rooted agent. Rooted agents stay inert unless this is
    /// deliberately asked for; `koru.bat root` uses it for its interactive
    /// development stack. Start-at-login remains unavailable.
    pub tray: bool,
    /// Run with no window: the display-free supervisor for a Bot on a server a
    /// person operates (`crate::server`). **Only this flag turns it on**: no
    /// environment variable and no file does, so it cannot be reached by
    /// accident. [`parse`] refuses it without `--root`, with `--tray`, with an
    /// update flag, and with any command but `run`.
    pub server: bool,
    /// Optional Koru profile selector, valid only for `open`.
    pub open_profile: Option<String>,
    /// Keep an Agent-launched Koru child's output on Agent's console. This is
    /// the development runner's path for live performance diagnostics.
    pub inherit_stdio: bool,
    /// The updater's one-time health token, when this launch is the restart
    /// that follows an install.
    ///
    /// **The flag has to exist even where nothing reads it**, which is why it
    /// is here rather than being sniffed off `std::env::args` somewhere later:
    /// the updater restarts the agent as
    /// `koru-agent.exe --update-health-token <token>`, and a parser that does
    /// not know the flag names it, prints the usage and **exits 2** — the agent
    /// would never come up, the updater's `wait_for_health` would see the child
    /// gone, and the whole update would roll back. A strict parser is the right
    /// rule and this is the argument it must not be strict about.
    ///
    /// `None` covers two different things and deliberately does not distinguish
    /// them: not given, and given in a shape that is not a token. See
    /// [`health_token`].
    pub update_health_token: Option<String>,
    /// A normal signed manifest beside a local package, offered without any
    /// update-feed request. Valid only with an explicit root on `run`.
    pub rehearse_update: Option<PathBuf>,
}

struct Flag {
    name: &'static str,
    short: Option<&'static str>,
    takes_value: bool,
    args: &'static str,
    help: &'static str,
}

/// The single table. Parsing reads it and so does the help.
const FLAGS: &[Flag] = &[
    Flag {
        name: "--root",
        short: None,
        takes_value: true,
        args: "--root <path>",
        help: "use this directory instead of the channel's; machine integration stays off unless --tray opts into input",
    },
    Flag {
        name: "--console",
        short: None,
        takes_value: false,
        args: "--console",
        help: "keep the console and stay out of the notification area",
    },
    Flag {
        name: "--tray",
        short: None,
        takes_value: false,
        args: "--tray",
        help: "show a rooted Agent tray icon and enable its global shortcuts",
    },
    Flag {
        name: "--server",
        short: None,
        takes_value: false,
        args: "--server",
        help: "run with no window, tray or account, for a Bot on a server (requires --root)",
    },
    Flag {
        name: "--profile",
        short: None,
        takes_value: true,
        args: "--profile <name|id|path>",
        help: "open Koru on this profile (valid only with the open command)",
    },
    Flag {
        name: "--inherit-stdio",
        short: None,
        takes_value: false,
        args: "--inherit-stdio",
        help: "keep an opened Koru child's diagnostics on this console",
    },
    Flag {
        name: "--update-health-token",
        short: None,
        takes_value: true,
        args: "--update-health-token <token>",
        help: "confirm to the updater that this build came up (the updater passes it)",
    },
    Flag {
        name: "--rehearse-update",
        short: None,
        takes_value: true,
        args: "--rehearse-update <manifest>",
        help: "offer a locally signed update manifest inside an explicit --root",
    },
    Flag {
        name: "--version",
        short: Some("-V"),
        takes_value: false,
        args: "--version",
        help: "print the version and exit",
    },
    Flag {
        name: "--help",
        short: Some("-h"),
        takes_value: false,
        args: "--help",
        help: "print this message and exit",
    },
];

const COMMANDS: &[(&str, &str)] = &[
    ("run", "run the agent (the default)"),
    ("status", "report on the running agent"),
    ("bots", "read the Bots engine diagnostic from its database"),
    (
        "delegate",
        "what the delegate worker holds (a run id prints that run)",
    ),
    ("open", "focus or launch a Koru workspace window"),
    ("pair", "display a short-lived device-pairing QR"),
    ("pair-cancel", "cancel the current pairing invitation"),
    (
        "rehearse-download",
        "accept the download step of an explicit-root update rehearsal",
    ),
    (
        "rehearse-install",
        "accept the install step of an explicit-root update rehearsal",
    ),
    ("stop", "ask the running agent to stop"),
    (
        "debug",
        "print a store diagnostic report, e.g. debug documents/<id>",
    ),
    (
        "register-url-scheme",
        "handle koru:// links with this build",
    ),
    ("unregister-url-scheme", "stop handling koru:// links"),
    ("open-url", "follow a koru:// link (used by the shell)"),
    (
        "notification-activation",
        "forward an opaque native activation (used by the operating system)",
    ),
];

#[derive(Debug)]
pub struct Rejected {
    pub arg: String,
    pub why: Why,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Why {
    Unknown { did_you_mean: Option<&'static str> },
    MissingValue,
    /// Understood, and not allowed in this combination. The text finishes the
    /// sentence that starts with the argument's name.
    Refused(&'static str),
}

impl Rejected {
    pub fn message(&self) -> String {
        match &self.why {
            Why::Unknown {
                did_you_mean: Some(near),
            } => format!("unknown argument `{}` — did you mean `{near}`?", self.arg),
            Why::Unknown { did_you_mean: None } => format!("unknown argument `{}`", self.arg),
            Why::MissingValue => format!("`{}` needs a value", self.arg),
            Why::Refused(reason) => format!("`{}` {reason}", self.arg),
        }
    }

    /// Say what was wrong, print the usage, exit 2.
    pub fn report(&self) -> ! {
        eprintln!("koru-agent: {}", self.message());
        eprintln!();
        eprint!("{}", help());
        std::process::exit(2)
    }
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Invocation, Rejected> {
    let tokens: Vec<String> = args.into_iter().skip(1).collect();
    let mut command: Option<Command> = None;
    let mut root: Option<PathBuf> = None;
    let mut console = false;
    let mut tray = false;
    let mut server = false;
    // Seen, whatever its value: a malformed health token parses to `None`.
    let mut update_flag = false;
    let mut open_profile = None;
    let mut inherit_stdio = false;
    let mut update_health_token: Option<String> = None;
    let mut rehearse_update: Option<PathBuf> = None;
    let mut i = 0;

    while i < tokens.len() {
        let token = tokens[i].clone();

        // `--help` and `--version` win wherever they appear, so
        // `koru-agent run --help` explains rather than starting one.
        if token == "--help" || token == "-h" {
            return Ok(Invocation {
                command: Command::Help,
                root,
                console,
                tray,
                server,
                open_profile,
                inherit_stdio,
                update_health_token,
                rehearse_update,
            });
        }
        if token == "--version" || token == "-V" {
            return Ok(Invocation {
                command: Command::Version,
                root,
                console,
                tray,
                server,
                open_profile,
                inherit_stdio,
                update_health_token,
                rehearse_update,
            });
        }

        if token.starts_with("--") {
            let (name, inline) = match token[2..].split_once('=') {
                Some((n, v)) => (format!("--{n}"), Some(v.to_string())),
                None => (token.clone(), None),
            };
            let Some(flag) = FLAGS
                .iter()
                .find(|f| f.name == name || f.short == Some(name.as_str()))
            else {
                return Err(Rejected {
                    why: Why::Unknown {
                        did_you_mean: nearest(&name),
                    },
                    arg: name,
                });
            };
            if flag.takes_value {
                let value = match inline {
                    Some(v) => {
                        i += 1;
                        v
                    }
                    None => {
                        let Some(v) = tokens.get(i + 1) else {
                            return Err(Rejected {
                                arg: name,
                                why: Why::MissingValue,
                            });
                        };
                        i += 2;
                        v.clone()
                    }
                };
                if flag.name == "--root" {
                    root = Some(PathBuf::from(value));
                } else if flag.name == "--update-health-token" {
                    update_flag = true;
                    update_health_token = health_token(&value);
                } else if flag.name == "--rehearse-update" {
                    update_flag = true;
                    rehearse_update = Some(PathBuf::from(value));
                } else if flag.name == "--profile" {
                    open_profile = Some(value);
                }
            } else {
                if flag.name == "--console" {
                    console = true;
                } else if flag.name == "--tray" {
                    tray = true;
                } else if flag.name == "--server" {
                    server = true;
                } else if flag.name == "--inherit-stdio" {
                    inherit_stdio = true;
                }
                i += 1;
            }
            continue;
        }

        // A bare word is a subcommand.
        let chosen = match token.as_str() {
            "run" => Command::Run,
            "status" => Command::Status,
            "bots" => Command::Bots,
            "delegate" => {
                // Like `projection`, an optional target follows the verb.
                let target = tokens.get(i + 1).filter(|next| !next.starts_with('-'));
                if target.is_some() {
                    i += 1;
                }
                Command::Delegate(target.cloned())
            }
            "open" => Command::Open,
            "pair" => Command::Pair,
            "pair-cancel" => Command::PairCancel,
            "rehearse-download" => Command::RehearseDownload,
            "rehearse-install" => Command::RehearseInstall,
            "stop" => Command::Stop,
            "debug" => {
                // Like `open-url`, this verb consumes what follows it: the
                // diagnostic path under `/v1/debug/`.
                let Some(path) = tokens.get(i + 1).cloned() else {
                    return Err(Rejected {
                        why: Why::MissingValue,
                        arg: token,
                    });
                };
                i += 1;
                Command::DebugStore(path)
            }
            "register-url-scheme" => Command::RegisterUrlScheme,
            "unregister-url-scheme" => Command::UnregisterUrlScheme,
            "open-url" => {
                // The one verb that consumes what follows it. A missing URL is
                // a rejection rather than a launch with nothing to open.
                let Some(url) = tokens.get(i + 1).cloned() else {
                    return Err(Rejected {
                        why: Why::MissingValue,
                        arg: token,
                    });
                };
                i += 1;
                Command::OpenUrl(url)
            }
            "notification-activation" => {
                let Some(envelope) = tokens.get(i + 1).cloned() else {
                    return Err(Rejected {
                        why: Why::MissingValue,
                        arg: token,
                    });
                };
                i += 1;
                Command::NotificationActivation(envelope)
            }
            _ => {
                return Err(Rejected {
                    why: Why::Unknown {
                        did_you_mean: nearest(&token),
                    },
                    arg: token,
                });
            }
        };
        command = Some(chosen);
        i += 1;
    }

    // **Running is the default**, because that is what a Run-key launch and
    // a double-click mean, and neither can pass an argument.
    let command = command.unwrap_or(Command::Run);
    if server {
        // Refused here rather than in `main`, so that no `Invocation` with
        // `server` set and one of these wrong can exist at all.
        let refusal = if command != Command::Run {
            Some("is valid only when running the Agent")
        } else if root.is_none() {
            // A server never uses the per-user root: that is a person's own
            // Koru, and this mode would start it with no window to reach it.
            Some("needs `--root <path>`: a server never uses the per-user root")
        } else if tray {
            Some("cannot be combined with `--tray`: a server has no notification area")
        } else if update_flag {
            Some("cannot be combined with an update flag: a server takes no update lease")
        } else {
            None
        };
        if let Some(reason) = refusal {
            return Err(Rejected {
                arg: "--server".into(),
                why: Why::Refused(reason),
            });
        }
    }

    Ok(Invocation {
        command,
        root,
        console,
        tray,
        server,
        open_profile,
        inherit_stdio,
        update_health_token,
        rehearse_update,
    })
}

/// The updater's health token, if it has the exact shape Koru generates.
///
/// Thirty-two lowercase hex characters and nothing else — the same rule, and
/// for the same reason, as `koru_workspace_host::cli::update_health_token_from_args`,
/// and the exact shape [`crate::update`] mints: a v4 UUID in `simple` form,
/// which is 32 hex digits with no dashes.
/// **The token names a file that gets written**, `<health dir>/<token>.ok`, so
/// an unvalidated one chooses *where*: `..\..\something` is a path, not a name,
/// and this argument reaches the process from outside it.
///
/// **A token that fails the check is treated as absent, not as a parse error**,
/// which is the one place this module bends its own rule. The strict parser
/// exists so a mistyped argument cannot quietly do the wrong thing; here the
/// wrong thing is *refusing to start*. An agent that exited 2 over a malformed
/// token would be an agent that did not come up, which is precisely the failure
/// the token is there to detect — and it would make a rejected token
/// indistinguishable from a broken build. Starting and simply never confirming
/// health lets the updater roll back on its own timer, which is the recoverable
/// outcome.
fn health_token(value: &str) -> Option<String> {
    (value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

/// The nearest flag or subcommand within three edits, or nothing.
///
/// Three, like the shell's: far enough to catch a transposition and a missing
/// letter, near enough that `--roost` does not suggest `--console`.
fn nearest(arg: &str) -> Option<&'static str> {
    let candidates = FLAGS
        .iter()
        .map(|f| f.name)
        .chain(COMMANDS.iter().map(|(name, _)| *name));
    candidates
        .map(|name| (distance(arg, name), name))
        .filter(|(d, _)| *d <= 3)
        .min_by_key(|(d, _)| *d)
        .map(|(_, name)| name)
}

/// Levenshtein, written out rather than pulled in: `strsim` is in the workspace
/// but only for the search ranking, and one twelve-line function is cheaper
/// than an edge from a CLI to a text-similarity crate.
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut row = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            row[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(row[j] + 1);
        }
        std::mem::swap(&mut prev, &mut row);
    }
    prev[b.len()]
}

pub fn version() -> String {
    format!("koru-agent {}", env!("CARGO_PKG_VERSION"))
}

pub fn help() -> String {
    let mut out = String::from(
        "koru-agent — the Koru Agent. It owns the device.\n\n\
         usage: koru-agent [command] [options]\n\n\
         commands:\n",
    );
    let command_width = COMMANDS
        .iter()
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or_default()
        + 2;
    for (name, help) in COMMANDS {
        out.push_str(&format!("  {name:<command_width$}{help}\n"));
    }
    out.push_str("\noptions:\n");
    let option_width = FLAGS
        .iter()
        .map(|flag| flag.args.len())
        .max()
        .unwrap_or_default()
        + 2;
    for flag in FLAGS {
        out.push_str(&format!("  {:<option_width$}{}\n", flag.args, flag.help));
    }
    out
}

#[cfg(test)]
mod tests;
