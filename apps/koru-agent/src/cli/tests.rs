// SPDX-License-Identifier: MPL-2.0

use super::*;

fn parse_args(args: &[&str]) -> Result<Invocation, Rejected> {
    parse(std::iter::once("koru-agent".to_string()).chain(args.iter().map(|a| a.to_string())))
}

#[test]
fn no_arguments_runs_the_agent() {
    // What a Run-key launch and a double-click mean, and neither can pass one.
    assert_eq!(parse_args(&[]).unwrap().command, Command::Run);
}

#[test]
fn an_unknown_argument_is_refused_and_never_ignored() {
    // **The rule.** A mistyped `--root` that was ignored would silently take
    // the hotkey, the tray and the Run key on the channel's own root — which is
    // exactly what naming a root exists to prevent.
    let rejected = parse_args(&["--roots", "/tmp/x"]).unwrap_err();
    assert_eq!(rejected.arg, "--roots");
    assert_eq!(
        rejected.why,
        Why::Unknown {
            did_you_mean: Some("--root")
        }
    );
    assert!(
        rejected.message().contains("did you mean"),
        "{}",
        rejected.message()
    );
}

#[test]
fn an_unknown_subcommand_is_refused_too() {
    let rejected = parse_args(&["ruc"]).unwrap_err();
    assert_eq!(
        rejected.why,
        Why::Unknown {
            did_you_mean: Some("run")
        }
    );
}

#[test]
fn something_nothing_resembles_suggests_nothing() {
    // Better than a confident wrong guess: `--roost` must not be answered with
    // `--console`.
    assert_eq!(
        parse_args(&["--wholly-unrelated"]).unwrap_err().why,
        Why::Unknown { did_you_mean: None }
    );
}

#[test]
fn a_flag_that_needs_a_value_and_has_none_says_so() {
    assert_eq!(parse_args(&["--root"]).unwrap_err().why, Why::MissingValue);
}

#[test]
fn a_root_arrives_both_ways_round() {
    // `--root x` and `--root=x` are the same thing, and somebody will type
    // each of them.
    for args in [vec!["--root", "/tmp/koru"], vec!["--root=/tmp/koru"]] {
        assert_eq!(
            parse_args(&args).unwrap().root,
            Some(PathBuf::from("/tmp/koru"))
        );
    }
}

#[test]
fn help_and_version_win_wherever_they_appear() {
    // So `koru-agent run --help` explains rather than starting an agent, which
    // is the one outcome nobody wants from asking a question.
    assert_eq!(
        parse_args(&["run", "--help"]).unwrap().command,
        Command::Help
    );
    assert_eq!(parse_args(&["-h"]).unwrap().command, Command::Help);
    assert_eq!(
        parse_args(&["run", "--version"]).unwrap().command,
        Command::Version
    );
    assert_eq!(parse_args(&["-V"]).unwrap().command, Command::Version);
}

#[test]
fn every_command_and_flag_parses() {
    // The table is the only declaration, so this is what keeps the parser and
    // the help from drifting: anything documented must be accepted.
    for (name, _) in COMMANDS {
        // These verbs consume what follows
        // them, so the table alone cannot say how to invoke them.
        let args = match *name {
            "open-url" => vec![*name, "koru://doc/00000000-0000-4000-8000-000000000000"],
            "notification-activation" => vec![*name, "opaque"],
            "debug" => vec![*name, "documents/00000000-0000-4000-8000-000000000000"],
            _ => vec![*name],
        };
        assert!(parse_args(&args).is_ok(), "{name}");
    }
    for flag in FLAGS {
        let args = if flag.takes_value {
            vec![flag.name, "/tmp/value"]
        } else if flag.name == "--server" {
            // The one flag that is refused alone: it needs a root. See
            // `server_mode_needs_an_explicit_root`.
            vec![flag.name, "--root", "/tmp/value"]
        } else {
            vec![flag.name]
        };
        assert!(parse_args(&args).is_ok(), "{}", flag.name);
    }
}

/// `delegate` takes an optional run id, the way `projection [<id>]` does:
/// bare it reports the whole ledger, and a flag after it is a flag rather
/// than a target.
#[test]
fn delegate_takes_an_optional_run_id() {
    assert_eq!(
        parse_args(&["delegate"]).unwrap().command,
        Command::Delegate(None)
    );
    let run = "00000000-0000-4000-8000-000000000001";
    let rooted = parse_args(&["delegate", run, "--root", "/tmp/x"]).unwrap();
    assert_eq!(rooted.command, Command::Delegate(Some(run.to_string())));
    assert_eq!(rooted.root, Some(PathBuf::from("/tmp/x")));
    assert_eq!(
        parse_args(&["--root", "/tmp/x", "delegate"])
            .unwrap()
            .command,
        Command::Delegate(None)
    );
}

#[test]
fn every_documented_flag_appears_in_the_help() {
    // The other direction: anything accepted must be documented.
    let text = help();
    for flag in FLAGS {
        assert!(
            text.contains(flag.args),
            "{} missing from the help",
            flag.args
        );
    }
    for (name, _) in COMMANDS {
        assert!(text.contains(name), "{name} missing from the help");
    }
}

#[test]
fn every_command_description_is_separated_from_its_name() {
    let help = help();
    for (name, description) in COMMANDS {
        assert!(
            help.lines().any(|line| line.trim_start().starts_with(name)
                && line.contains(&format!("  {description}"))),
            "command {name} ran into its description"
        );
    }
}

#[test]
fn every_option_description_is_separated_from_its_arguments() {
    let help = help();
    for flag in FLAGS {
        assert!(
            help.lines()
                .any(|line| line.trim_start().starts_with(flag.args)
                    && line.contains(&format!("  {}", flag.help))),
            "option {} ran into its description",
            flag.args
        );
    }
}

#[test]
fn console_is_off_unless_asked_for() {
    // A tray launch has no console to stay in. Defaulting it on would put a
    // window in front of somebody who asked for a background service.
    assert!(!parse_args(&[]).unwrap().console);
    assert!(parse_args(&["--console"]).unwrap().console);
}

#[test]
fn the_updaters_restart_argument_is_understood() {
    // **The one that would have cost an update.** The updater restarts the
    // agent as `koru-agent.exe --update-health-token <token>`; a parser that did
    // not know the flag would name it, print the usage and exit 2, the
    // updater's `wait_for_health` would see the child gone, and a good install
    // would roll back. Both spellings, because a value flag has two.
    let token = "0123456789abcdef0123456789abcdef";
    let inline = format!("--update-health-token={token}");
    for args in [vec!["--update-health-token", token], vec![inline.as_str()]] {
        let parsed = parse_args(&args).unwrap();
        assert_eq!(parsed.update_health_token.as_deref(), Some(token));
        // And it is still an ordinary run: the token says nothing about which
        // verb this is.
        assert_eq!(parsed.command, Command::Run);
    }
}

#[test]
fn a_token_that_is_not_a_token_is_absent_rather_than_fatal() {
    // The token names the file that gets written, `<health dir>/<token>.ok`, so
    // an unvalidated one chooses *where*. But refusing to start is the one
    // failure this argument must not cause — that is exactly what a missing
    // health file already means — so a bad token starts the agent and simply
    // never confirms it, and the updater rolls back on its own timer.
    for bad in [
        "..\\..\\evil",
        "0123456789abcdef0123456789abcde",   // 31
        "0123456789abcdef0123456789abcdef0", // 33
        "0123456789abcdef0123456789abcdeg",  // not hex
        "",
    ] {
        let parsed = parse_args(&["--update-health-token", bad]).unwrap();
        assert_eq!(parsed.update_health_token, None, "{bad}");
        assert_eq!(parsed.command, Command::Run, "{bad}");
    }
}

#[test]
fn a_token_is_kept_in_one_spelling() {
    // Lowercased on the way in, because the updater watches for a filename and
    // one of the two filesystems this runs on cares about the difference.
    assert_eq!(
        parse_args(&["--update-health-token", "0123456789ABCDEF0123456789ABCDEF"])
            .unwrap()
            .update_health_token
            .as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );
}

#[test]
fn a_root_tray_is_opt_in() {
    assert!(!parse_args(&["--root", "/tmp/koru"]).unwrap().tray);
    assert!(parse_args(&["--root", "/tmp/koru", "--tray"]).unwrap().tray);
}

#[test]
fn open_is_a_root_qualified_client_command() {
    let parsed = parse_args(&[
        "open",
        "--root",
        "C:/isolated-koru",
        "--profile",
        "Research",
        "--inherit-stdio",
    ])
    .unwrap();
    assert_eq!(parsed.command, Command::Open);
    assert_eq!(parsed.root, Some(PathBuf::from("C:/isolated-koru")));
    assert_eq!(parsed.open_profile.as_deref(), Some("Research"));
    assert!(parsed.inherit_stdio);
}

#[test]
fn the_local_update_manifest_is_a_strict_value_flag() {
    let parsed = parse_args(&[
        "run",
        "--root",
        "C:/isolated-koru",
        "--rehearse-update=C:/releases/b.update.json",
    ])
    .unwrap();
    assert_eq!(parsed.command, Command::Run);
    assert_eq!(parsed.root, Some(PathBuf::from("C:/isolated-koru")));
    assert_eq!(
        parsed.rehearse_update,
        Some(PathBuf::from("C:/releases/b.update.json"))
    );
    assert_eq!(
        parse_args(&["--rehearse-update"]).unwrap_err().why,
        Why::MissingValue
    );
}

#[test]
fn rooted_rehearsal_consent_verbs_parse_without_changing_the_root() {
    for (verb, command) in [
        ("rehearse-download", Command::RehearseDownload),
        ("rehearse-install", Command::RehearseInstall),
    ] {
        let parsed = parse_args(&[verb, "--root", "C:/isolated-koru"]).unwrap();
        assert_eq!(parsed.command, command);
        assert_eq!(parsed.root, Some(PathBuf::from("C:/isolated-koru")));
    }
}

#[test]
fn open_url_takes_the_url_that_follows_it() {
    let url = "koru://doc/00000000-0000-4000-8000-000000000000";
    assert_eq!(
        parse_args(&["open-url", url]).unwrap().command,
        Command::OpenUrl(url.to_owned())
    );
}

#[test]
fn mac_notification_owner_argument_order_reaches_the_selected_singleton() {
    let parsed = parse_args(&[
        "notification-activation",
        "opaque",
        "--root",
        "/private/root",
    ])
    .unwrap();
    assert_eq!(
        parsed.command,
        Command::NotificationActivation("opaque".into())
    );
    assert_eq!(parsed.root, Some(PathBuf::from("/private/root")));
}

/// A verb with nothing to act on is a rejection, not a launch with nothing to
/// open — the registry writes this command line, and a malformed one should
/// say so rather than quietly starting an agent.
#[test]
fn open_url_without_a_url_is_rejected() {
    assert!(parse_args(&["open-url"]).is_err());
}

/// **The strict parser keeps its teeth.** Accepting a bare first argument
/// because it happened to look like a URL is exactly the guess `cli.rs` exists
/// to refuse; the registered command names the verb explicitly.
#[test]
fn a_bare_url_without_the_verb_is_rejected() {
    assert!(parse_args(&["koru://doc/00000000-0000-4000-8000-000000000000"]).is_err());
}

#[test]
fn the_url_scheme_verbs_parse() {
    assert_eq!(
        parse_args(&["register-url-scheme"]).unwrap().command,
        Command::RegisterUrlScheme
    );
    assert_eq!(
        parse_args(&["unregister-url-scheme"]).unwrap().command,
        Command::UnregisterUrlScheme
    );
}

/// **A server never uses the per-user root.** That root is a person's own
/// Koru; started this way it would have no window to reach it. Refused by the
/// parser, with the usage and exit 2, like any other argument that cannot be
/// run as written.
#[test]
fn server_mode_needs_an_explicit_root() {
    let rejected = parse_args(&["--server"]).unwrap_err();
    assert_eq!(rejected.arg, "--server");
    assert!(matches!(rejected.why, Why::Refused(_)), "{:?}", rejected.why);
    assert!(
        rejected.message().contains("--root"),
        "{}",
        rejected.message()
    );
    // The explicit verb changes nothing.
    assert!(parse_args(&["run", "--server"]).is_err());

    // Both orders, and both spellings of a value flag.
    for args in [
        vec!["--server", "--root", "/srv/koru"],
        vec!["--root", "/srv/koru", "--server"],
        vec!["--root=/srv/koru", "run", "--server"],
    ] {
        let parsed = parse_args(&args).unwrap();
        assert!(parsed.server, "{args:?}");
        assert_eq!(parsed.command, Command::Run);
        assert_eq!(parsed.root, Some(PathBuf::from("/srv/koru")));
        assert!(!parsed.tray);
    }
}

/// Server mode is reached by its flag and by nothing else: not by default, not
/// by naming a root, not by any other flag. (No environment variable or file
/// is read by the parser at all, so none can turn it on.)
#[test]
fn server_mode_is_off_unless_its_flag_is_given() {
    assert!(!parse_args(&[]).unwrap().server);
    assert!(!parse_args(&["run"]).unwrap().server);
    assert!(!parse_args(&["--root", "/srv/koru"]).unwrap().server);
    assert!(!parse_args(&["--root", "/srv/koru", "--console"]).unwrap().server);
    assert!(!parse_args(&["--root", "/srv/koru", "--tray"]).unwrap().server);
    // A near miss is an unknown argument with a suggestion, never a guess.
    let rejected = parse_args(&["--root", "/srv/koru", "--servr"]).unwrap_err();
    assert_eq!(
        rejected.why,
        Why::Unknown {
            did_you_mean: Some("--server")
        }
    );
}

/// What a server does not have, it does not accept: a notification area, an
/// update lease, or any verb but `run`.
#[test]
fn server_mode_refuses_what_a_server_does_not_have() {
    let root = ["--root", "/srv/koru", "--server"];
    let token = "0123456789abcdef0123456789abcdef";
    for extra in [
        vec!["--tray"],
        vec!["--update-health-token", token],
        // A token in the wrong shape is absent as a value; the flag was still
        // given, and it is the flag a server refuses.
        vec!["--update-health-token", "not-a-token"],
        vec!["--rehearse-update", "/tmp/manifest.json"],
        vec!["status"],
        vec!["stop"],
        vec!["open"],
        vec!["delegate"],
    ] {
        let mut args = root.to_vec();
        args.extend(extra.iter().copied());
        let rejected = parse_args(&args).unwrap_err();
        assert_eq!(rejected.arg, "--server", "{args:?}");
        assert!(matches!(rejected.why, Why::Refused(_)), "{args:?}");
    }
    // The console is not a display: a service manager reads standard output.
    let parsed = parse_args(&["--root", "/srv/koru", "--server", "--console"]).unwrap();
    assert!(parsed.server && parsed.console);
    // Asking a question still wins, as it does everywhere else.
    assert_eq!(
        parse_args(&["--server", "--help"]).unwrap().command,
        Command::Help
    );
}

#[test]
fn the_help_lists_server_mode_and_what_it_requires() {
    let text = help();
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with("--server"))
        .expect("the help has no --server line");
    assert!(line.contains("--root"), "{line}");
    assert!(line.contains("no window"), "{line}");
}
