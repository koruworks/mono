// SPDX-License-Identifier: MPL-2.0

//! **A command line that is not understood is not run.**
//!
//! The rule and the shape of `apps/koru-voice/src/cli.rs`: one table that both
//! the parser and the help read, so a flag cannot be accepted without being
//! documented. An unknown argument names itself, suggests the nearest real
//! flag within three edits, prints the usage and exits 2.
//!
//! Here it matters for the reason it matters for voice: a mistyped `--root`
//! would make this worker bind the *channel's* endpoint rather than a test
//! root's, and a rooted Store would never find it while the person's own Store
//! might.
//!
//! The worker has no verbs. It is started by the Store, as
//! `koru-read-worker --root <store dir> --supervised`, and does one thing.

use std::path::PathBuf;

/// What this invocation is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    /// Run the worker. What the Store launches.
    Serve,
    Help,
    Version,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Invocation {
    pub command: Command,
    /// The Store root this worker serves, instead of the channel's own. It
    /// reaches the endpoint name, so a rooted Store and its worker meet.
    pub root: Option<PathBuf>,
    /// Read the supervisor's bootstrap line from stdin before serving, and
    /// treat the end of stdin as the parent's death.
    pub supervised: bool,
    /// Log to the console as well as to the file.
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
        help: "the Store root to serve, instead of the channel's",
    },
    Flag {
        name: "--supervised",
        short: None,
        takes_value: false,
        args: "--supervised",
        help: "read the Store's launch bootstrap from stdin",
    },
    Flag {
        name: "--console",
        short: None,
        takes_value: false,
        args: "--console",
        help: "log to the console as well as to the rotating file",
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

#[derive(Debug, PartialEq, Eq)]
pub struct Rejected {
    pub arg: String,
    pub why: Why,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Why {
    Unknown { did_you_mean: Option<&'static str> },
    MissingValue,
    Repeated,
}

impl Rejected {
    pub fn message(&self) -> String {
        match &self.why {
            Why::Unknown {
                did_you_mean: Some(near),
            } => format!("unknown argument `{}` — did you mean `{near}`?", self.arg),
            Why::Unknown { did_you_mean: None } => format!("unknown argument `{}`", self.arg),
            Why::MissingValue => format!("`{}` needs a value", self.arg),
            Why::Repeated => format!("`{}` is given twice", self.arg),
        }
    }

    /// Say what was wrong, print the usage, exit 2.
    pub fn report(&self) -> ! {
        eprintln!("koru-read-worker: {}", self.message());
        eprintln!();
        eprint!("{}", help());
        std::process::exit(2)
    }
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Invocation, Rejected> {
    let tokens: Vec<String> = args.into_iter().skip(1).collect();
    let mut invocation = Invocation {
        command: Command::Serve,
        root: None,
        supervised: false,
        console: false,
    };
    let mut at = 0;
    while at < tokens.len() {
        let token = &tokens[at];
        // `--help` and `--version` win wherever they appear.
        if token == "--help" || token == "-h" {
            invocation.command = Command::Help;
            return Ok(invocation);
        }
        if token == "--version" || token == "-V" {
            invocation.command = Command::Version;
            return Ok(invocation);
        }
        let (name, inline) = match token
            .strip_prefix("--")
            .and_then(|rest| rest.split_once('='))
        {
            Some((name, value)) => (format!("--{name}"), Some(value.to_string())),
            None => (token.clone(), None),
        };
        let Some(flag) = FLAGS
            .iter()
            .find(|flag| flag.name == name || flag.short == Some(name.as_str()))
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
                Some(value) => {
                    at += 1;
                    value
                }
                None => {
                    let Some(value) = tokens.get(at + 1) else {
                        return Err(Rejected {
                            arg: name,
                            why: Why::MissingValue,
                        });
                    };
                    at += 2;
                    value.clone()
                }
            };
            if value.is_empty() {
                return Err(Rejected {
                    arg: name,
                    why: Why::MissingValue,
                });
            }
            if invocation.root.replace(PathBuf::from(value)).is_some() {
                return Err(Rejected {
                    arg: name,
                    why: Why::Repeated,
                });
            }
        } else {
            match flag.name {
                "--supervised" => invocation.supervised = true,
                "--console" => invocation.console = true,
                _ => {}
            }
            at += 1;
        }
    }
    Ok(invocation)
}

/// The nearest flag within three edits, so a typo is corrected rather than
/// merely refused.
fn nearest(name: &str) -> Option<&'static str> {
    FLAGS
        .iter()
        .map(|flag| (flag.name, distance(name, flag.name)))
        .filter(|(_, edits)| *edits <= 3)
        .min_by_key(|(_, edits)| *edits)
        .map(|(name, _)| name)
}

/// Levenshtein over characters, not bytes.
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
    format!("koru-read-worker {}", env!("CARGO_PKG_VERSION"))
}

/// Built from [`FLAGS`], so it cannot drift from the parser.
pub fn help() -> String {
    let mut out = String::from(
        "Koru read worker — handed a path to one file, it sends back the file's\n\
         words page by page. It holds no Store session and makes no network call.\n\n\
         Usage: koru-read-worker [options]\n\nOptions:\n",
    );
    for flag in FLAGS {
        let spelling = match flag.short {
            Some(short) => format!("{}, {short}", flag.args),
            None => flag.args.to_string(),
        };
        out.push_str(&format!("  {spelling:<22} {}\n", flag.help));
    }
    out.push_str(&format!(
        "\nChannels: {}\nThis build uses `{}`.\n",
        koru_channel::SEGMENTS.join(", "),
        koru_channel::segment()
    ));
    out.push_str(
        "\nThe Store starts the worker when it has a file to read, and the worker\n\
         ends itself after a spell with nothing to do. Starting one by hand is a\n\
         diagnostic, not a step anybody needs.\n",
    );
    out
}

#[cfg(test)]
mod tests;
