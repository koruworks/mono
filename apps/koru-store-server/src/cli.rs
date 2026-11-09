// SPDX-License-Identifier: MPL-2.0

//! **A command line that is not understood is not run.**
//!
//! The same rule, and the same shape, as `crates/koru-workspace-host/src/cli.rs`: one
//! table that both the parser and the help read, so a flag cannot be accepted
//! without being documented or documented without being accepted. An unknown
//! argument names itself, suggests the nearest real flag, and exits 2.
//!
//! Ignoring an unrecognised argument sounds harmless and is the same failure as
//! guessing a directory: a mistyped `--root` would quietly operate on the
//! *default* store rather than the one that was named, which for a tool that
//! can stop a daemon and restore backups is not a small mistake.

use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    /// Run the daemon. The default when no subcommand is given.
    Serve,
    /// What the read model holds and what it is doing. With a document id,
    /// print that document as the projection has it.
    Projection(Option<String>),
    /// What the index holds, and what any text finds in it.
    Search(Option<String>),
    /// What is outstanding in the read model. With a lifecycle — `open`,
    /// `completed` or `all` — filter the list.
    Tasks(Option<String>),
    /// Write one portable file containing every authoritative Store byte.
    Backup(Option<PathBuf>),
    /// Replace a stopped Store from a backup file.
    Restore(PathBuf),
    /// The root catalog's inventory as ids and counts, read-only.
    RootCatalog,
    Help,
    Version,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Invocation {
    pub command: Command,
    /// Override the store root. Chiefly for tests and for running a second
    /// store deliberately; a normal launch resolves it from the channel.
    pub root: Option<PathBuf>,
    /// Stay in the console instead of going to the notification area.
    ///
    /// **The developer's and the test harness's switch.** A normal launch —
    /// double-clicked, or from the Run key — has no console to stay in, puts an
    /// icon in the tray and writes to a log file. This asks for the opposite of
    /// all three, because a person watching a store start wants to see it
    /// start.
    pub console: bool,
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
        help: "use this directory as the store root instead of the channel's",
    },
    Flag {
        name: "--console",
        short: None,
        takes_value: false,
        args: "--console",
        help: "keep the console and stay out of the notification area",
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
    ("serve", "run the store (the default)"),
    (
        "projection",
        "what the read model holds (an id prints that document)",
    ),
    ("search", "what the index holds (any text searches it)"),
    (
        "tasks",
        "what is outstanding (open, completed or all filters it)",
    ),
    (
        "backup",
        "write the whole Store to one file (Store must be stopped)",
    ),
    (
        "restore",
        "restore one Store backup (Store must be stopped)",
    ),
    (
        "root-catalog",
        "the root catalog as ids and counts, read-only (never migrates)",
    ),
];

#[derive(Debug)]
pub struct Rejected {
    pub arg: String,
    pub why: Why,
}

#[derive(Debug)]
pub enum Why {
    Unknown { did_you_mean: Option<&'static str> },
    MissingValue,
}

impl Rejected {
    pub fn message(&self) -> String {
        match &self.why {
            Why::Unknown {
                did_you_mean: Some(near),
            } => format!("unknown argument `{}` — did you mean `{near}`?", self.arg),
            Why::Unknown { did_you_mean: None } => format!("unknown argument `{}`", self.arg),
            Why::MissingValue => format!("`{}` needs a value", self.arg),
        }
    }

    /// Say what was wrong, print the usage, exit 2.
    pub fn report(&self) -> ! {
        eprintln!("koru-store-server: {}", self.message());
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
    let mut i = 0;

    while i < tokens.len() {
        let token = tokens[i].clone();

        // `--help` and `--version` win wherever they appear, so `koru-store-server
        // serve --help` explains rather than starts a daemon.
        if token == "--help" || token == "-h" {
            return Ok(Invocation {
                command: Command::Help,
                root,
                console,
            });
        }
        if token == "--version" || token == "-V" {
            return Ok(Invocation {
                command: Command::Version,
                root,
                console,
            });
        }

        if let Some(rest) = token.strip_prefix("--") {
            let (name, inline) = match rest.split_once('=') {
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
                }
            } else {
                if flag.name == "--console" {
                    console = true;
                }
                i += 1;
            }
            continue;
        }

        // A bare word is a subcommand.
        let chosen = match token.as_str() {
            "serve" => Command::Serve,
            "projection" => {
                let target = tokens.get(i + 1).filter(|next| !next.starts_with('-'));
                if target.is_some() {
                    i += 1;
                }
                Command::Projection(target.cloned())
            }
            "search" => {
                let target = tokens.get(i + 1).filter(|next| !next.starts_with('-'));
                if target.is_some() {
                    i += 1;
                }
                Command::Search(target.cloned())
            }
            "tasks" => {
                let target = tokens.get(i + 1).filter(|next| !next.starts_with('-'));
                if target.is_some() {
                    i += 1;
                }
                Command::Tasks(target.cloned())
            }
            "backup" => {
                let target = tokens.get(i + 1).filter(|next| !next.starts_with('-'));
                if target.is_some() {
                    i += 1;
                }
                Command::Backup(target.map(PathBuf::from))
            }
            "restore" => {
                let Some(target) = tokens.get(i + 1).filter(|next| !next.starts_with('-')) else {
                    return Err(Rejected {
                        arg: "restore".into(),
                        why: Why::MissingValue,
                    });
                };
                i += 1;
                Command::Restore(PathBuf::from(target))
            }
            "root-catalog" => Command::RootCatalog,
            _ => {
                return Err(Rejected {
                    why: Why::Unknown {
                        did_you_mean: nearest_command(&token),
                    },
                    arg: token,
                });
            }
        };
        if command.is_some() {
            return Err(Rejected {
                why: Why::Unknown { did_you_mean: None },
                arg: token,
            });
        }
        command = Some(chosen);
        i += 1;
    }

    Ok(Invocation {
        command: command.unwrap_or(Command::Serve),
        root,
        console,
    })
}

/// The nearest flag within three edits, so a typo is corrected rather than
/// merely refused.
fn nearest(name: &str) -> Option<&'static str> {
    FLAGS
        .iter()
        .map(|f| (f.name, distance(name, f.name)))
        .filter(|(_, d)| *d <= 3)
        .min_by_key(|(_, d)| *d)
        .map(|(n, _)| n)
}

fn nearest_command(name: &str) -> Option<&'static str> {
    COMMANDS
        .iter()
        .map(|(c, _)| (*c, distance(name, c)))
        .filter(|(_, d)| *d <= 3)
        .min_by_key(|(_, d)| *d)
        .map(|(n, _)| n)
}

/// Levenshtein, over **characters not bytes** — a mistyped flag in any script
/// should still find its neighbour, and byte distance over UTF-8 measures
/// encoding rather than typing.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        current[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            current[j] = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

pub fn version() -> String {
    format!("koru-store-server {}", env!("CARGO_PKG_VERSION"))
}

/// Built from [`FLAGS`] and [`COMMANDS`], so it cannot drift from the parser.
pub fn help() -> String {
    let mut out = String::from(
        "Koru Store — the only component that persists Koru's CRDT state.\n\n\
         Usage: koru-store-server [<command>] [options]\n\n\
         Commands:\n",
    );
    for (name, help) in COMMANDS {
        out.push_str(&format!("  {name:<10} {help}\n"));
    }
    out.push_str("\nOptions:\n");
    for flag in FLAGS {
        // The short spelling is listed because it is *accepted*. A flag that
        // works and is not documented is the same drift the table exists to
        // prevent, just in the other direction.
        let spelling = match flag.short {
            Some(short) => format!("{}, {short}", flag.args),
            None => flag.args.to_string(),
        };
        out.push_str(&format!("  {spelling:<20} {}\n", flag.help));
    }
    out.push_str(
        "\nThis binary is an unshipped integration/diagnostic host. Product startup\n\
         uses Agent, whose gateway worker hosts the Store. A test harness must\n\
         provide the private readiness path through KORU_INTERNAL_STORE_READY.\n",
    );
    // Listed because the first question anyone asks of a store is "where is my
    // data", and the answer is always "under this channel's directory".
    // Channels never share databases; the same workspace in two of them is two
    // replicas that will one day converge through ordinary sync.
    out.push_str(&format!(
        "\nChannels: {}\nThis build uses `{}`.\n",
        koru_channel::SEGMENTS.join(", "),
        koru_channel::segment()
    ));
    out
}

#[cfg(test)]
mod tests;
