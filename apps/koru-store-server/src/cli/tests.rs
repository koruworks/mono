// SPDX-License-Identifier: MPL-2.0

use super::*;

fn parse_args(args: &[&str]) -> Result<Invocation, Rejected> {
    let mut all = vec!["koru-store-server".to_string()];
    all.extend(args.iter().map(|s| s.to_string()));
    parse(all)
}

#[test]
fn no_arguments_serves() {
    let i = parse_args(&[]).unwrap();
    assert_eq!(i.command, Command::Serve);
    assert_eq!(i.root, None);
}

#[test]
fn each_subcommand_parses() {
    for (word, expected) in [("serve", Command::Serve)] {
        assert_eq!(parse_args(&[word]).unwrap().command, expected);
    }
}

#[test]
fn the_root_can_be_given_either_way() {
    for args in [&["--root", "/tmp/store"][..], &["--root=/tmp/store"][..]] {
        let i = parse_args(args).unwrap();
        assert_eq!(i.root, Some(PathBuf::from("/tmp/store")));
    }
}

/// The reader is supervised only when asked: the black-box suites run this
/// host without the flag and must never start a worker beside a test binary.
#[test]
fn reading_is_off_unless_asked() {
    assert!(!parse_args(&[]).unwrap().read);
    assert!(!parse_args(&["serve", "--console"]).unwrap().read);
    let i = parse_args(&["serve", "--console", "--read", "--root", "/tmp/store"]).unwrap();
    assert!(i.read);
    assert!(i.console);
    assert_eq!(i.command, Command::Serve);
}

#[test]
fn a_root_combines_with_a_subcommand() {
    let i = parse_args(&["serve", "--root", "/tmp/store"]).unwrap();
    assert_eq!(i.command, Command::Serve);
    assert_eq!(i.root, Some(PathBuf::from("/tmp/store")));
}

#[test]
fn a_root_with_spaces_and_non_latin_characters_is_kept_whole() {
    // A path is bytes the operating system gave us, not something to validate.
    let path = r"C:\Users\سامي\Koru データ";
    assert_eq!(
        parse_args(&["--root", path]).unwrap().root,
        Some(PathBuf::from(path))
    );
}

#[test]
fn help_and_version_win_wherever_they_appear() {
    // `koru-store-server serve --help` must explain, not start a daemon.
    assert_eq!(parse_args(&["--help"]).unwrap().command, Command::Help);
    assert_eq!(parse_args(&["-h"]).unwrap().command, Command::Help);
    assert_eq!(
        parse_args(&["serve", "--help"]).unwrap().command,
        Command::Help
    );
    assert_eq!(
        parse_args(&["--version"]).unwrap().command,
        Command::Version
    );
    assert_eq!(parse_args(&["-V"]).unwrap().command, Command::Version);
    assert_eq!(
        parse_args(&["serve", "-V"]).unwrap().command,
        Command::Version
    );
}

#[test]
fn an_unknown_flag_is_refused_and_a_near_miss_is_suggested() {
    // Ignoring it would be the same failure as guessing a directory: a
    // mistyped `--root` would quietly operate on the default store.
    let e = parse_args(&["--roo", "/tmp"]).unwrap_err();
    assert!(matches!(
        e.why,
        Why::Unknown {
            did_you_mean: Some("--root")
        }
    ));
    assert!(e.message().contains("--root"), "{}", e.message());
}

#[test]
fn a_flag_with_no_neighbour_is_refused_without_a_suggestion() {
    let e = parse_args(&["--completely-different"]).unwrap_err();
    assert!(matches!(e.why, Why::Unknown { did_you_mean: None }));
}

#[test]
fn a_mistyped_subcommand_suggests_the_real_one() {
    let e = parse_args(&["serv"]).unwrap_err();
    assert!(matches!(
        e.why,
        Why::Unknown {
            did_you_mean: Some("serve")
        }
    ));
}

#[test]
fn a_flag_that_needs_a_value_and_has_none_says_so() {
    let e = parse_args(&["--root"]).unwrap_err();
    assert!(matches!(e.why, Why::MissingValue));
    assert!(e.message().contains("needs a value"), "{}", e.message());
}

#[test]
fn two_subcommands_are_refused() {
    assert!(parse_args(&["serve", "projection"]).is_err());
}

#[test]
fn every_documented_flag_parses_and_every_parsed_flag_is_documented() {
    // The guard that keeps the table honest: a flag cannot be accepted without
    // being documented, or documented without being accepted.
    let text = help();
    for flag in FLAGS {
        assert!(text.contains(flag.name), "{} is not in the help", flag.name);
        let args: Vec<&str> = if flag.takes_value {
            vec![flag.name, "value"]
        } else {
            vec![flag.name]
        };
        assert!(parse_args(&args).is_ok(), "{} does not parse", flag.name);
        if let Some(short) = flag.short {
            assert!(text.contains(short), "{short} is not in the help");
        }
    }
}

#[test]
fn every_documented_command_parses() {
    let text = help();
    for (name, _) in COMMANDS {
        assert!(text.contains(name), "{name} is not in the help");
        let args = if *name == "restore" {
            vec![*name, "backup.koru-store"]
        } else {
            vec![*name]
        };
        assert!(parse_args(&args).is_ok(), "{name} does not parse");
    }
}

#[test]
fn backup_has_an_optional_destination_and_restore_requires_one() {
    assert_eq!(
        parse_args(&["backup"]).unwrap().command,
        Command::Backup(None)
    );
    assert_eq!(
        parse_args(&["backup", "safe.koru-store"]).unwrap().command,
        Command::Backup(Some(PathBuf::from("safe.koru-store")))
    );
    assert!(parse_args(&["restore"]).is_err());
    assert_eq!(
        parse_args(&["restore", "safe.koru-store"]).unwrap().command,
        Command::Restore(PathBuf::from("safe.koru-store"))
    );
}

#[test]
fn the_help_says_the_standalone_host_is_internal() {
    let text = help();
    assert!(
        text.contains("unshipped integration/diagnostic host"),
        "{text}"
    );
    assert!(text.contains("KORU_INTERNAL_STORE_READY"), "{text}");
    // And it must not still describe the two files that were deleted. Help
    // naming a file nothing writes any more sends someone looking in a
    // directory for something that is not there — which is a worse failure
    // than no documentation, because it reads as authoritative.
    assert!(!text.contains("store.json"), "{text}");
    assert!(!text.contains("store.secret"), "{text}");
}

#[test]
fn edit_distance_counts_characters_not_bytes() {
    // A mistyped flag in any script should still find its neighbour; byte
    // distance over UTF-8 measures encoding rather than typing.
    assert_eq!(distance("root", "root"), 0);
    assert_eq!(distance("roo", "root"), 1);
    assert_eq!(distance("計画", "計面"), 1);
    assert_eq!(distance("état", "etat"), 1);
}
