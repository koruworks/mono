// SPDX-License-Identifier: MPL-2.0

use super::*;

fn args(words: &[&str]) -> Vec<String> {
    std::iter::once("koru-read-worker")
        .chain(words.iter().copied())
        .map(String::from)
        .collect()
}

/// **The exact command line the Store launches.**
#[test]
fn the_supervised_launch_parses() {
    let invocation = parse(args(&["--root", "C:/store", "--supervised"])).unwrap();
    assert_eq!(
        invocation,
        Invocation {
            command: Command::Serve,
            root: Some(PathBuf::from("C:/store")),
            supervised: true,
            console: false,
        }
    );
    let inline = parse(args(&["--root=C:/store", "--supervised"])).unwrap();
    assert_eq!(inline, invocation);
}

#[test]
fn nothing_at_all_serves_on_the_channel_root() {
    let invocation = parse(args(&[])).unwrap();
    assert_eq!(invocation.command, Command::Serve);
    assert_eq!(invocation.root, None);
    assert!(!invocation.supervised);
}

#[test]
fn a_mistyped_flag_is_refused_and_corrected() {
    let rejected = parse(args(&["--rot", "x"])).unwrap_err();
    assert_eq!(rejected.arg, "--rot");
    assert_eq!(
        rejected.why,
        Why::Unknown {
            did_you_mean: Some("--root")
        }
    );
    assert!(rejected.message().contains("--root"));
}

/// The worker has no verbs; a bare word is not quietly ignored.
#[test]
fn a_bare_word_is_refused() {
    let rejected = parse(args(&["serve"])).unwrap_err();
    assert_eq!(rejected.arg, "serve");
}

#[test]
fn a_root_without_a_value_is_refused() {
    assert_eq!(parse(args(&["--root"])).unwrap_err().why, Why::MissingValue);
    assert_eq!(
        parse(args(&["--root="])).unwrap_err().why,
        Why::MissingValue
    );
}

#[test]
fn a_second_root_is_refused_rather_than_one_winning() {
    assert_eq!(
        parse(args(&["--root", "a", "--root", "b"]))
            .unwrap_err()
            .why,
        Why::Repeated
    );
}

#[test]
fn help_and_version_win_anywhere() {
    assert_eq!(
        parse(args(&["--supervised", "--help"])).unwrap().command,
        Command::Help
    );
    assert_eq!(parse(args(&["-V"])).unwrap().command, Command::Version);
}

/// The help is built from the table, so every flag the parser accepts is in
/// it.
#[test]
fn every_flag_is_documented() {
    let text = help();
    for flag in FLAGS {
        assert!(text.contains(flag.args), "{} is not in the help", flag.name);
        let spelled: Vec<&str> = if flag.takes_value {
            vec![flag.name, "value"]
        } else {
            vec![flag.name]
        };
        assert!(
            parse(args(&spelled)).is_ok(),
            "{} is documented and refused",
            flag.name
        );
    }
}
