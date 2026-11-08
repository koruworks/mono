// SPDX-License-Identifier: MPL-2.0

//! The command line: what Koru accepts, and what it says when you get it wrong.
//!
//! **A command line that is not understood is not run.** Ignoring an argument
//! it does not recognise is how a program opens the wrong workspace: a typo in
//! `--profile` used to be silently dropped, and the app would then ask for a
//! folder — or worse, open the remembered one — as if nothing had happened. A
//! flag that was meant to point somewhere and did not is exactly the case where
//! carrying on is the dangerous answer.
//!
//! So an unrecognised argument is a hard stop: name the offending token,
//! suggest the nearest real flag when there is one, print the usage, and exit
//! **2** — the same code `koru.sh` uses for an unknown subcommand.
//!
//! [`FLAGS`] is the single table. Parsing reads it and so does the help, so a
//! flag cannot be accepted without being documented or documented without being
//! accepted.

use std::path::PathBuf;

use crate::settings::MIN_WINDOW_SIZE;

/// What the process was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Open the app, which is nearly always the answer.
    Run,
    Help,
    Version,
    /// `--backup [<path>]`. `None` is the default destination.
    Backup(Option<PathBuf>),
    /// `--where`: which folder this launch would use, and why.
    Where,
    /// `--profiles`: every profile this machine knows about, and whether its
    /// folder is still there.
    Profiles,
    /// `--envs`: every environment variable Koru reads, from `koru-env`'s
    /// registry — the binary answers so the table can never be a stale copy
    /// in a script.
    Envs,
}

/// One accepted flag. `value` is what it needs after it, and is `None` for a
/// flag that takes nothing.
struct Flag {
    name: &'static str,
    /// The short spelling, where there is one.
    short: Option<&'static str>,
    value: Value,
    /// How it is shown in the usage block.
    args: &'static str,
    help: &'static [&'static str],
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Value {
    None,
    Required,
    Optional,
}

const FLAGS: &[Flag] = &[
    Flag {
        name: "--peer",
        short: None,
        value: Value::Required,
        args: "--peer <name>",
        help: &[
            "Run as a named replica with its own window geometry and its own",
            "update file. Launch two and watch them converge; F5 toggles",
            "sharing. Without it, each window gets its own update file",
            "automatically and shares the default layout. Not a way to",
            "separate content - that is --profile.",
        ],
    },
    Flag {
        name: "--window-size",
        short: None,
        value: Value::Required,
        args: "--window-size <WxH>",
        help: &[
            "Open the workspace window at this inner size, in logical pixels.",
            "Example: 1024x768. Overrides the remembered size for this launch",
            "and does not maximise. Smaller than 720x600 is refused.",
        ],
    },
    Flag {
        name: "--workspace",
        short: None,
        value: Value::Required,
        args: "--workspace <id>",
        help: &[
            "Open this workspace inside the profile, instead of the one this",
            "window had open last. Ignored if the profile does not list it -",
            "an id from another profile names content in another folder.",
        ],
    },
    Flag {
        name: "--profile",
        short: None,
        value: Value::Required,
        args: "--profile <name|id|path>",
        help: &[
            "Open a profile by name, id, or folder path. A name or id is looked",
            "up in the list; an existing path is adopted as a profile. Refused",
            "if a name matches nothing, or more than one - choosing between two",
            "you might have meant is the same failure as guessing a folder.",
        ],
    },
    Flag {
        name: "--store-profile",
        short: None,
        value: Value::Required,
        args: "--store-profile <id>",
        help: &[
            "Open this profile from the Koru Store. Used by the in-app profile",
            "switcher while the legacy --profile startup path is removed.",
        ],
    },
    Flag {
        name: "--root",
        short: None,
        value: Value::Required,
        args: "--root <path>",
        help: &[
            "Keep this build's data under this directory instead of the",
            "channel's shared one. It holds store/ and agent/ beside each other,",
            "exactly as the channel's own root does.",
            "",
            "The same path the agent takes; koru-store-server is given the store/",
            "inside it, because a daemon names its own directory. All of them",
            "must agree: the endpoint they meet on carries a digest of the root,",
            "so a mismatched --root does not connect to the wrong store - it",
            "finds no store at all.",
            "",
            "This is how a build under target/ is kept off the content a",
            "released build writes, which is the rule profiles.conf already",
            "follows by living beside the exe.",
        ],
    },
    Flag {
        name: "--profiles",
        short: None,
        value: Value::None,
        args: "--profiles",
        help: &[
            "List every profile Koru knows about, with its folder and",
            "whether that folder is still there. Then exit.",
        ],
    },
    Flag {
        name: "--pick-profile",
        short: None,
        value: Value::None,
        args: "--pick-profile",
        help: &[
            "Ask which profile to open, instead of reopening the last one.",
            "Holding Shift while Koru starts does the same thing (Windows",
            "only; elsewhere use this flag).",
        ],
    },
    Flag {
        name: "--background",
        short: None,
        value: Value::None,
        args: "--background",
        help: &[
            "Start Koru in the notification area so global search is ready",
            "without opening a workspace window.",
        ],
    },
    Flag {
        name: "--no-search",
        short: None,
        value: Value::None,
        args: "--no-search",
        help: &[
            "Start without the global search surface. The Koru Store still",
            "indexes; this window simply does not offer search, and opens no",
            "session for one. Intended for diagnosis.",
        ],
    },
    Flag {
        name: "--test-upgrade",
        short: None,
        value: Value::None,
        args: "--test-upgrade",
        help: &[
            "Show a safe, simulated ready-to-install update. Intended for",
            "checking update UI and interaction without downloading or replacing files.",
        ],
    },
    Flag {
        name: "--new-document",
        short: None,
        value: Value::None,
        args: "--new-document",
        help: &[
            "Open Koru with the new-document prompt. Used by the Windows",
            "taskbar action.",
        ],
    },
    Flag {
        name: "--search",
        short: None,
        value: Value::None,
        args: "--search",
        help: &[
            "Open the global search overlay. Used by the Windows taskbar's",
            "Search action.",
        ],
    },
    Flag {
        name: "--open-document",
        short: None,
        value: Value::Required,
        args: "--open-document <id>",
        help: &[
            "Open this stable document id after resolving its current profile",
            "and workspace. Used for internal cross-process navigation.",
        ],
    },
    Flag {
        name: "--open",
        short: None,
        value: Value::Required,
        args: "--open <id>",
        help: &[
            "Open this document and put it in front. Used by the Windows",
            "taskbar's Recent list; a Koru that is already running takes the",
            "request rather than a second process starting.",
        ],
    },
    Flag {
        name: "--open-trash",
        short: None,
        value: Value::Required,
        args: "--open-trash <id>",
        help: &["Open this deleted-page root. Used by global search."],
    },
    Flag {
        name: "--new-window",
        short: None,
        value: Value::None,
        args: "--new-window",
        help: &[
            "Do not hand an open request to an existing process; create a new",
            "window instead. Used by global search Ctrl+Enter.",
        ],
    },
    Flag {
        name: "--update-health-token",
        short: None,
        value: Value::Required,
        args: "--update-health-token <token>",
        help: &["Internal updater health handshake. Not intended for normal use."],
    },
    Flag {
        name: "--reopen-claim",
        short: None,
        value: Value::Required,
        args: "--reopen-claim <token>",
        help: &["Internal crash-safe update reopen claim. Not intended for normal use."],
    },
    Flag {
        name: "--backup",
        short: None,
        value: Value::Optional,
        args: "--backup [<path>]",
        help: &[
            "Copy the whole workspace somewhere safe and exit. Defaults to",
            "koru-backups/<timestamp> beside it.",
        ],
    },
    Flag {
        name: "--where",
        short: None,
        value: Value::None,
        args: "--where",
        help: &[
            "Print which folder this launch would use, where that came from,",
            "and whether there is a workspace in it. Then exit.",
        ],
    },
    Flag {
        name: "--envs",
        short: None,
        value: Value::None,
        args: "--envs",
        help: &[
            "List every environment variable Koru reads - switches, values,",
            "compile-time stamps - with this session's values. Then exit.",
        ],
    },
    Flag {
        name: "--version",
        short: Some("-V"),
        value: Value::None,
        args: "--version, -V",
        help: &["Print the version and exit."],
    },
    Flag {
        name: "--help",
        short: Some("-h"),
        value: Value::None,
        args: "--help, -h",
        help: &["This message."],
    },
];

/// Why the command line was rejected, with the token to quote back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejected {
    pub arg: String,
    pub why: Why,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Why {
    /// Not a flag this build knows.
    Unknown {
        /// The closest real flag, when one is close enough to be worth saying.
        did_you_mean: Option<&'static str>,
    },
    /// A flag that needs a value, given none.
    MissingValue,
    /// A bare word where nothing was expecting one.
    Unexpected,
    /// A flag whose value is the wrong shape.
    InvalidValue {
        /// What a valid value looks like, quoted in the error.
        expected: &'static str,
    },
}

impl Rejected {
    /// The first line: what is wrong, and what to send instead.
    pub fn message(&self) -> String {
        match &self.why {
            Why::Unknown {
                did_you_mean: Some(m),
            } => format!("unrecognised option `{}` - did you mean `{m}`?", self.arg),
            Why::Unknown { did_you_mean: None } => {
                format!("unrecognised option `{}`", self.arg)
            }
            Why::MissingValue => format!("`{}` needs a value after it", self.arg),
            Why::Unexpected => format!(
                "unexpected argument `{}` - Koru takes options only, \
                 and opens the workspace it was pointed at",
                self.arg
            ),
            Why::InvalidValue { expected } => {
                format!("`{}` needs {expected}", self.arg)
            }
        }
    }

    /// Say it, then show what is accepted. Both go to stderr: this path always
    /// ends in a non-zero exit, and a shell that pipes stdout should still see
    /// the complaint.
    ///
    /// **Also as a dialog.** A windowless launch — a Jump List entry started by
    /// Explorer — has no console, so the stderr lines are written into nothing
    /// and a rejected command line would otherwise exit 2 in silence. The
    /// dialog is the one surface a person can actually see.
    pub fn report(&self, program: &str) {
        eprintln!("error: {}", self.message());
        eprintln!();
        eprintln!("{}", help(program));
        rfd::MessageDialog::new()
            .set_title("Koru")
            .set_level(rfd::MessageLevel::Error)
            .set_description(self.message())
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
    }
}

/// Parse a whole command line, `argv[0]` included.
///
/// `--help` and `--version` win wherever they appear: somebody who mistyped a
/// flag and reached for `--help` in the same breath should get help.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Command, Rejected> {
    let args: Vec<String> = args.into_iter().skip(1).collect();

    for a in &args {
        if a == "--help" || a == "-h" {
            return Ok(Command::Help);
        }
        if a == "--version" || a == "-V" {
            return Ok(Command::Version);
        }
    }

    let mut command = Command::Run;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        // `--flag=value`, which every value-taking flag accepts alongside the
        // separated form, because both spellings turn up in launcher scripts.
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (arg, None),
        };

        let Some(flag) = FLAGS
            .iter()
            .find(|f| f.name == name || f.short == Some(name))
        else {
            return Err(Rejected {
                arg: arg.to_string(),
                why: if arg.starts_with('-') {
                    Why::Unknown {
                        did_you_mean: nearest(name),
                    }
                } else {
                    Why::Unexpected
                },
            });
        };

        // How many tokens this flag consumed, and what its value was.
        let (value, step) = match (flag.value, inline) {
            (Value::None, Some(_)) => {
                // `--help=x`. Not worth a variant of its own; it is a flag that
                // does not exist in that spelling.
                return Err(Rejected {
                    arg: arg.to_string(),
                    why: Why::Unknown {
                        did_you_mean: Some(flag.name),
                    },
                });
            }
            (Value::None, None) => (None, 1),
            (_, Some(v)) if v.trim().is_empty() => {
                return Err(Rejected {
                    arg: name.to_string(),
                    why: Why::MissingValue,
                });
            }
            (_, Some(v)) => (Some(v.to_string()), 1),
            (Value::Required, None) => match args.get(i + 1) {
                // A flag where the value should be is a forgotten value, not a
                // directory called `--peer`.
                Some(v) if !v.starts_with('-') => (Some(v.clone()), 2),
                _ => {
                    return Err(Rejected {
                        arg: name.to_string(),
                        why: Why::MissingValue,
                    });
                }
            },
            (Value::Optional, None) => match args.get(i + 1) {
                Some(v) if !v.starts_with('-') => (Some(v.clone()), 2),
                _ => (None, 1),
            },
        };

        match flag.name {
            "--backup" => command = Command::Backup(value.map(PathBuf::from)),
            "--where" => command = Command::Where,
            "--profiles" => command = Command::Profiles,
            "--envs" => command = Command::Envs,
            "--window-size" => {
                if value
                    .as_deref()
                    .is_some_and(|size| parse_window_size(size).is_none())
                {
                    return Err(Rejected {
                        arg: flag.name.to_string(),
                        why: Why::InvalidValue {
                            expected: WINDOW_SIZE_EXPECTED,
                        },
                    });
                }
            }
            _ => {}
        }
        i += step;
    }
    Ok(command)
}

/// The closest flag to `typed`, if one is close enough to be worth suggesting.
///
/// Three edits covers the mistakes people actually make — a typo (`--profil`),
/// a stray plural (`--peers`), one dash instead of two — without confidently
/// pointing at something unrelated.
fn nearest(typed: &str) -> Option<&'static str> {
    FLAGS
        .iter()
        .map(|f| (distance(typed, f.name), f.name))
        .min_by_key(|(d, _)| *d)
        .filter(|(d, _)| *d <= 3)
        .map(|(_, name)| name)
}

/// Levenshtein, two rows rather than a matrix. Flag names are short; this is
/// run once, on the way to exiting.
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// What this build calls itself.
///
/// `KORU_VERSION` is set by the distribution runner from the tag it is packaging, so a
/// bundle can say which one it is. A plain `cargo build` has no tag to speak of
/// and falls back to the workspace version.
pub fn version() -> &'static str {
    option_env!("KORU_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// How this process was invoked, for the usage line.
pub fn program_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "koru".into())
}

/// The usage block, built from [`FLAGS`] so it cannot drift from what parses.
pub fn help(program: &str) -> String {
    let mut out = format!(
        "Koru {} - a task outline on a CRDT\n\nUsage: {program} [options]\n\n",
        version()
    );
    // One column wide enough for the widest spelling, so the descriptions line
    // up whatever is added to the table later.
    let col = FLAGS.iter().map(|f| f.args.len()).max().unwrap_or(0) + 2;
    for flag in FLAGS {
        for (n, line) in flag.help.iter().enumerate() {
            let label = if n == 0 { flag.args } else { "" };
            out.push_str(&format!("  {label:<col$}{line}\n"));
        }
    }
    out.push_str(
        r#"
A profile is a folder: the document, its backups, the images and this install's
window layout all live in it. The first run asks where; after that, the last
profile you opened reopens until you hold Shift or pass --profile.

Two binaries share one workspace:
  koru                 the manager - documents in one process
  koru-document        one document alone
"#,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(list: &[&str]) -> Vec<String> {
        std::iter::once("koru.exe")
            .chain(list.iter().copied())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn the_ordinary_launch_parses() {
        assert_eq!(parse(argv(&[])), Ok(Command::Run));
        assert_eq!(parse(argv(&["--peer", "A"])), Ok(Command::Run));
        assert_eq!(parse(argv(&["--peer=A"])), Ok(Command::Run));
        assert_eq!(
            parse(argv(&["--profile", "C:/work", "--peer", "B"])),
            Ok(Command::Run)
        );
        assert_eq!(parse(argv(&["--profile=C:/work"])), Ok(Command::Run));
        assert_eq!(parse(argv(&["--new-document"])), Ok(Command::Run));
        assert_eq!(parse(argv(&["--no-search"])), Ok(Command::Run));
        assert_eq!(parse(argv(&["--search"])), Ok(Command::Run));
        assert_eq!(parse(argv(&["--test-upgrade"])), Ok(Command::Run));
        assert_eq!(
            parse(argv(&["--window-size", "1024x768"])),
            Ok(Command::Run)
        );
        assert_eq!(parse(argv(&["--window-size=1920x1080"])), Ok(Command::Run));
    }

    #[test]
    fn window_size_reads_wxh_and_equals() {
        assert_eq!(
            window_size_from(argv(&["--window-size", "1024x768"])),
            Some((1024.0, 768.0))
        );
        assert_eq!(
            window_size_from(argv(&["--window-size=720x600"])),
            Some((720.0, 600.0))
        );
        assert_eq!(
            window_size_from(argv(&["--window-size", "1920,1080"])),
            Some((1920.0, 1080.0))
        );
        assert_eq!(
            window_size_from(argv(&["--window-size", "1024X768"])),
            Some((1024.0, 768.0))
        );
        assert_eq!(window_size_from(argv(&[])), None);
    }

    #[test]
    fn a_bad_window_size_is_refused_and_named() {
        for args in [
            vec!["--window-size", "wide"],
            vec!["--window-size", "1024"],
            vec!["--window-size", "719x600"],
            vec!["--window-size", "1024x599"],
            vec!["--window-size", "0x0"],
            vec!["--window-size", "1024x768x32"],
        ] {
            let bad = parse(argv(&args)).unwrap_err();
            assert_eq!(bad.arg, "--window-size", "for {args:?}");
            assert_eq!(
                bad.why,
                Why::InvalidValue {
                    expected: WINDOW_SIZE_EXPECTED
                },
                "for {args:?}"
            );
            assert!(
                bad.message().contains("1024x768"),
                "for {args:?}: {}",
                bad.message()
            );
        }
    }

    #[test]
    fn a_store_profile_id_parses_without_becoming_a_folder_profile() {
        let id = koru_core::ProfileId::new();
        assert_eq!(
            store_profile_from(argv(&["--store-profile", &id.to_storage()])),
            Some(id)
        );
        assert_eq!(
            store_profile_from(argv(&[&format!("--store-profile={id}")])),
            Some(id)
        );
        assert_eq!(
            profile_from(argv(&["--store-profile", &id.to_storage()])),
            None
        );
        assert_eq!(
            store_profile_from(argv(&["--store-profile", "not-an-id"])),
            None
        );
    }

    #[test]
    fn help_and_version_win_wherever_they_are() {
        assert_eq!(parse(argv(&["--help"])), Ok(Command::Help));
        assert_eq!(parse(argv(&["-h"])), Ok(Command::Help));
        assert_eq!(parse(argv(&["--version"])), Ok(Command::Version));
        assert_eq!(parse(argv(&["-V"])), Ok(Command::Version));
        // Mistyped something and reached for help in the same breath.
        assert_eq!(parse(argv(&["--nonsense", "--help"])), Ok(Command::Help));
        assert_eq!(
            parse(argv(&["--peer", "A", "--version"])),
            Ok(Command::Version)
        );
    }

    #[test]
    fn backup_takes_an_optional_path() {
        assert_eq!(parse(argv(&["--backup"])), Ok(Command::Backup(None)));
        assert_eq!(
            parse(argv(&["--backup", "E:/keep"])),
            Ok(Command::Backup(Some(PathBuf::from("E:/keep"))))
        );
        assert_eq!(
            parse(argv(&["--backup=E:/keep"])),
            Ok(Command::Backup(Some(PathBuf::from("E:/keep"))))
        );
        // A following flag is not the destination.
        assert_eq!(
            parse(argv(&["--backup", "--peer", "A"])),
            Ok(Command::Backup(None))
        );
    }

    /// The whole point: a command line that is not understood is not run.
    /// Silently dropping a mistyped `--profile` is how you open the wrong
    /// workspace and never find out.
    #[test]
    fn an_unknown_option_is_refused_and_named() {
        let bad = parse(argv(&["--profil", "Work"])).unwrap_err();
        assert_eq!(bad.arg, "--profil");
        assert_eq!(
            bad.why,
            Why::Unknown {
                did_you_mean: Some("--profile")
            }
        );
        assert!(bad.message().contains("--profil"), "{}", bad.message());
        assert!(bad.message().contains("--profile"), "{}", bad.message());
    }

    /// The near-misses people actually type.
    #[test]
    fn the_suggestion_catches_the_usual_slips() {
        for (typed, expected) in [
            ("--profil", "--profile"),
            ("-profile", "--profile"),
            ("--peers", "--peer"),
            ("--Peer", "--peer"),
            ("-peer", "--peer"),
            ("--bakup", "--backup"),
            ("--verson", "--version"),
            ("--window-siz", "--window-size"),
        ] {
            assert_eq!(nearest(typed), Some(expected), "for {typed}");
        }
        // Nothing close enough. Better to say nothing than to point somewhere
        // unrelated with confidence.
        assert_eq!(nearest("--frobnicate"), None);
    }

    /// A flag that needs a path and did not get one must not fall through to
    /// "no profile named, so ask" - the user thinks they named one.
    #[test]
    fn a_flag_missing_its_value_is_refused() {
        for args in [
            vec!["--profile"],
            vec!["--profile="],
            vec!["--profile", "--peer", "A"],
            vec!["--peer"],
            vec!["--peer="],
            vec!["--window-size"],
            vec!["--window-size="],
            vec!["--window-size", "--peer", "A"],
        ] {
            let bad = parse(argv(&args)).unwrap_err();
            assert_eq!(bad.why, Why::MissingValue, "for {args:?}");
            assert!(bad.message().contains("needs a value"), "for {args:?}");
        }
    }

    #[test]
    fn a_bare_word_is_refused() {
        let bad = parse(argv(&["notes.md"])).unwrap_err();
        assert_eq!(bad.why, Why::Unexpected);
        assert!(bad.message().contains("notes.md"), "{}", bad.message());
    }

    /// The table is the only source, so every flag it lists has to parse and
    /// every one that parses has to be listed. A flag added to one and not the
    /// other fails here rather than in somebody's terminal.
    #[test]
    fn every_documented_flag_parses() {
        let text = help("koru");
        for flag in FLAGS {
            assert!(text.contains(flag.name), "{} is not in the help", flag.name);
            let args = match flag.value {
                Value::Required if flag.name == "--window-size" => {
                    vec![flag.name, "1024x768"]
                }
                Value::Required => vec![flag.name, "x"],
                _ => vec![flag.name],
            };
            assert!(
                parse(argv(&args)).is_ok(),
                "{} is documented but does not parse",
                flag.name
            );
            if let Some(short) = flag.short {
                assert!(text.contains(short), "{short} is not in the help");
            }
        }
    }

    /// The help is what an error prints, so it has to name the version and the
    /// program that produced it.
    #[test]
    fn the_help_says_what_it_is() {
        let text = help("koru-document");
        assert!(text.contains(version()), "{text}");
        assert!(text.contains("Usage: koru-document"), "{text}");
    }
}

/// The `--profile <name|id>` flag, if it was given.
pub fn profile_from_args() -> Option<String> {
    profile_from(std::env::args())
}

/// The testable half of [`profile_from_args`].
pub fn profile_from<I: IntoIterator<Item = String>>(args: I) -> Option<String> {
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        if let Some(v) = arg.strip_prefix("--profile=") {
            return non_empty(v);
        }
        // `--profiles` and `--pick-profile` are their own flags; an exact match
        // is the only thing that counts here.
        if arg == "--profile" {
            return args.next().as_deref().and_then(non_empty);
        }
    }
    None
}

/// The Store profile selected by an in-app profile switch.
pub fn store_profile_from_args() -> Option<koru_core::ProfileId> {
    store_profile_from(std::env::args())
}

/// The testable half of [`store_profile_from_args`].
pub fn store_profile_from<I: IntoIterator<Item = String>>(args: I) -> Option<koru_core::ProfileId> {
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--store-profile=") {
            return koru_core::ProfileId::parse(value);
        }
        if arg == "--store-profile" {
            return koru_core::ProfileId::parse(&args.next()?);
        }
    }
    None
}

/// The `--root <path>` flag, if it was given — **a channel root, not a profile
/// folder and not the store's own directory.**
///
/// It is the same value `koru-agent --root` takes, and it holds `store/` and
/// `agent/` beside each other exactly as the default channel root does. Use
/// [`store_root_from_args`] to reach the store inside it; this raw form is for
/// the agent, whose endpoint is derived from the channel root.
///
/// Passing it to only some processes is not a partial failure:
/// `Endpoint::for_service_root` appends a digest of the root to the endpoint
/// name, so a shell with a root and a store without one look for different names
/// and simply never meet. That is the intended shape — a mismatch that connected
/// to the *wrong* store would be far worse than one that connects to nothing.
///
/// Not canonicalised. The digest is over the string, so the three processes have
/// to be given the *same spelling*; `koru.bat` is what guarantees that for a dev
/// build, and a person passing it by hand copies the same path.
pub fn root_from_args() -> Option<PathBuf> {
    root_from(std::env::args())
}

/// The store inside the `--root` this launch was given.
///
/// `koru_channel::store_root_under` is the only place that join happens — see
/// it for why an explicit root is a *channel* root. **`koru-store-server --root` is
/// one level down from this**, because the daemon names its own directory; the
/// shell and the agent both pass it this value rather than their own.
pub fn store_root_from_args() -> Option<PathBuf> {
    store_root_from(std::env::args())
}

/// The testable half of [`store_root_from_args`].
pub fn store_root_from<I: IntoIterator<Item = String>>(args: I) -> Option<PathBuf> {
    root_from(args).map(|root| koru_channel::store_root_under(&root))
}

/// The testable half of [`root_from_args`].
pub fn root_from<I: IntoIterator<Item = String>>(args: I) -> Option<PathBuf> {
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        if let Some(v) = arg.strip_prefix("--root=") {
            return non_empty(v).map(PathBuf::from);
        }
        if arg == "--root" {
            return args
                .next()
                .as_deref()
                .and_then(non_empty)
                .map(PathBuf::from);
        }
    }
    None
}

/// Whether this launch was told to ask which profile to open.
pub fn pick_profile_from_args() -> bool {
    pick_profile_from(std::env::args())
}

/// The testable half of [`pick_profile_from_args`].
pub fn pick_profile_from<I: IntoIterator<Item = String>>(args: I) -> bool {
    args.into_iter().skip(1).any(|a| a == "--pick-profile")
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// What `--window-size` quotes in its error, and in the help.
const WINDOW_SIZE_EXPECTED: &str = "a size like 1024x768 (at least 720x600)";

/// Largest inner size `--window-size` (and a remembered row) will honour.
/// Same ceiling `Settings` uses for a `w=`/`h=` line.
const MAX_WINDOW_SIZE: f32 = 20_000.0;

/// Inner client size from `--window-size <WxH>`, if it was given and parses.
///
/// Logical pixels, the same unit a remembered window stores. A malformed or
/// out-of-range value is `None` here because [`parse`] has already refused it
/// on the way in.
pub fn window_size_from_args() -> Option<(f32, f32)> {
    window_size_from(std::env::args())
}

/// The testable half of [`window_size_from_args`].
pub fn window_size_from<I: IntoIterator<Item = String>>(args: I) -> Option<(f32, f32)> {
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        if let Some(v) = arg.strip_prefix("--window-size=") {
            return parse_window_size(v);
        }
        if arg == "--window-size" {
            return args.next().as_deref().and_then(parse_window_size);
        }
    }
    None
}

/// `WIDTHxHEIGHT` (either case) or `WIDTH,HEIGHT`.
///
/// Whole pixels only. Below the minimum window size or above 20000 is refused
/// rather than clamped: a size on the command line is a request, and shrinking
/// it silently is the same failure as opening the wrong folder.
fn parse_window_size(value: &str) -> Option<(f32, f32)> {
    let value = value.trim();
    let (width, height) = value.split_once(['x', 'X', ','])?;
    let width = parse_window_edge(width, MIN_WINDOW_SIZE.0)?;
    let height = parse_window_edge(height, MIN_WINDOW_SIZE.1)?;
    Some((width, height))
}

fn parse_window_edge(value: &str, min: f32) -> Option<f32> {
    let n: u32 = value.trim().parse().ok()?;
    let n = n as f32;
    (min..=MAX_WINDOW_SIZE).contains(&n).then_some(n)
}

/// The `--workspace <id>` flag, if it was given and parses.
///
/// A malformed id is `None` rather than an error: the strict parser above has
/// already accepted the *flag*, and what it names is looked up in the profile's
/// list, which is where an id that means nothing here is refused.
pub fn workspace_from_args() -> Option<koru_core::WorkspaceId> {
    workspace_from(std::env::args())
}

/// Whether this launch should become a tray/background process after its
/// initial state is loaded.
pub fn background_from_args() -> bool {
    std::env::args().any(|arg| arg == "--background")
}

/// The testable half of [`workspace_from_args`].
pub fn workspace_from<I: IntoIterator<Item = String>>(args: I) -> Option<koru_core::WorkspaceId> {
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        if let Some(v) = arg.strip_prefix("--workspace=") {
            return koru_core::WorkspaceId::parse(v);
        }
        if arg == "--workspace" {
            return koru_core::WorkspaceId::parse(&args.next()?);
        }
    }
    None
}

/// The updater's one-time health token, if it has the exact safe spelling Koru
/// generates. Rejecting path punctuation keeps remote or hand-edited arguments
/// from choosing where the health file is written.
pub fn update_health_token_from_args() -> Option<String> {
    update_health_token_from(std::env::args())
}

fn update_health_token_from<I: IntoIterator<Item = String>>(args: I) -> Option<String> {
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        let value = if let Some(value) = arg.strip_prefix("--update-health-token=") {
            Some(value.to_string())
        } else if arg == "--update-health-token" {
            args.next()
        } else {
            None
        };
        if let Some(value) = value {
            return (value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .then(|| value.to_ascii_lowercase());
        }
    }
    None
}
