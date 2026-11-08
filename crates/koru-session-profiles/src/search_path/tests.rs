// SPDX-License-Identifier: MPL-2.0

use super::*;

fn known(name: &str) -> Option<String> {
    match name {
        "USERPROFILE" => Some(r"C:\Users\x".into()),
        "SystemRoot" => Some(r"C:\Windows".into()),
        _ => None,
    }
}

/// **A `REG_EXPAND_SZ` `Path` reads as Windows would expand it**: known names
/// replaced, unknown ones and stray `%` kept as written.
#[test]
fn windows_variables_expand_like_expand_environment_strings() {
    assert_eq!(
        expand_windows_variables(r"%USERPROFILE%\.local\bin;%SystemRoot%\system32", known),
        r"C:\Users\x\.local\bin;C:\Windows\system32"
    );
    assert_eq!(
        expand_windows_variables(r"%NOPE%\bin", known),
        r"%NOPE%\bin",
        "an unknown name is text"
    );
    assert_eq!(expand_windows_variables("100%", known), "100%");
    assert_eq!(expand_windows_variables("%%", known), "%%");
    assert_eq!(
        expand_windows_variables(r"%NOPE%%USERPROFILE%", known),
        r"%NOPE%C:\Users\x",
        "an unknown name's closing % may open a known one"
    );
    assert_eq!(expand_windows_variables("", known), "");
    assert_eq!(expand_windows_variables("plain", known), "plain");
}

#[test]
fn a_windows_path_splits_on_semicolons_outside_quotes() {
    assert_eq!(
        split_windows_path(r#"C:\a;;"C:\b;c" ; C:\d\;"#),
        [
            PathBuf::from(r"C:\a"),
            PathBuf::from(r"C:\b;c"),
            PathBuf::from(r"C:\d\")
        ]
    );
    assert!(split_windows_path("").is_empty());
    assert!(split_windows_path(";;").is_empty());
}

/// **The inherited `PATH` keeps its order and precedence; the registry only
/// adds what it lacks**, compared as Windows compares directories.
#[test]
fn the_current_path_follows_the_inherited_one_without_repeats() {
    let inherited = vec![
        PathBuf::from(r"C:\Windows\system32"),
        PathBuf::from(r"C:\Tools\"),
    ];
    let current = vec![
        PathBuf::from(r"c:\windows\System32"),
        PathBuf::from(r"C:\Users\x\.local\bin"),
        PathBuf::from(r"C:\tools"),
        PathBuf::from(r"C:\Users\x\AppData\Roaming\npm"),
        PathBuf::from(r"C:\Users\x\.local\bin\"),
    ];
    assert_eq!(
        merge_windows(inherited, current),
        [
            PathBuf::from(r"C:\Windows\system32"),
            PathBuf::from(r"C:\Tools\"),
            PathBuf::from(r"C:\Users\x\.local\bin"),
            PathBuf::from(r"C:\Users\x\AppData\Roaming\npm"),
        ]
    );
    assert_eq!(
        merge_windows(Vec::new(), vec![PathBuf::from(r"C:\a")]),
        [PathBuf::from(r"C:\a")],
        "no inherited PATH at all still searches the registry's"
    );
}

/// The registry read itself, on the machine running the suite: it may find
/// nothing, but whatever it finds is a non-empty value, and the merged
/// search still begins with the inherited `PATH`.
#[cfg(windows)]
#[test]
fn the_registry_path_is_read_and_follows_the_inherited_one() {
    for value in registry_paths() {
        assert!(!value.is_empty());
    }
    let inherited = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    let merged = directories();
    let unique_inherited = merge_windows(inherited, Vec::new());
    assert_eq!(merged[..unique_inherited.len()], unique_inherited[..]);
}

/// **Only what is between the markers is the `PATH`**: a profile's noise
/// before and after it is not, and an answer missing either marker is no
/// answer.
#[test]
fn the_login_path_is_read_between_its_markers() {
    let printed = format!(
        "Last login: Thu on ttys001\nnvm: using node 22\n{PATH_BEGIN}/opt/homebrew/bin:/usr/bin{PATH_END}\nlogout\n"
    );
    assert_eq!(
        marked_path(printed.as_bytes()),
        Some(&b"/opt/homebrew/bin:/usr/bin"[..])
    );
    assert_eq!(
        marked_path(format!("{PATH_BEGIN}{PATH_END}").as_bytes()),
        Some(&b""[..]),
        "an empty PATH is an answer, and adds nothing"
    );
    assert_eq!(marked_path(b""), None);
    assert_eq!(marked_path(b"/usr/bin:/bin"), None, "no markers");
    assert_eq!(
        marked_path(format!("{PATH_BEGIN}/opt/homebrew/bin:/us").as_bytes()),
        None,
        "cut off before the closing marker"
    );
    assert_eq!(
        marked_path(format!("/usr/bin{PATH_END}").as_bytes()),
        None,
        "a closing marker alone"
    );
}

/// **The inherited `PATH` keeps its order; the login shell and the
/// installers only add what it lacks**, and never a relative entry.
// Unix paths: `/usr/bin` is not absolute on Windows.
#[cfg(unix)]
#[test]
fn unix_extras_follow_the_inherited_path_without_repeats() {
    let inherited = vec![
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/opt/homebrew/bin/"),
    ];
    let extra = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from(""),
        PathBuf::from("."),
        PathBuf::from("node_modules/.bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/Users/x/.local/bin"),
        PathBuf::from("/usr/local/bin/"),
    ];
    assert_eq!(
        merge_unix(inherited, extra),
        [
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
            PathBuf::from("/opt/homebrew/bin/"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/Users/x/.local/bin"),
        ]
    );
    assert_eq!(
        merge_unix(Vec::new(), vec![PathBuf::from("/a")]),
        [PathBuf::from("/a")],
        "no inherited PATH at all still searches the extras"
    );
}

/// **The installers' directories, then the login shell's, then Homebrew's
/// fallback** — each installer directory only when it is there, Homebrew's
/// only on a Mac, the per-user ones only under an absolute home.
// Unix paths: `/usr/bin` is not absolute on Windows.
#[cfg(unix)]
#[test]
fn extras_are_the_installers_then_the_login_shell_then_homebrew() {
    let home = Path::new("/Users/x");
    let login = [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/bin"),
    ];
    let everything = |_: &Path| true;
    assert_eq!(
        extra_directories(Some(home), true, &login, everything),
        [
            home.join(".local/bin"),
            home.join(".opencode/bin"),
            home.join(".grok/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ],
        "merge_unix drops the repeat"
    );
    let only_local = |directory: &Path| directory.ends_with(".local/bin");
    assert_eq!(
        extra_directories(Some(home), false, &login, only_local),
        [
            home.join(".local/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/bin"),
        ],
        "a missing installer directory is left out; the login shell's are not checked"
    );
    assert!(extra_directories(None, false, &[], everything).is_empty());
    assert!(
        extra_directories(Some(Path::new("relative")), false, &[], everything).is_empty(),
        "a relative HOME would make every entry relative"
    );
    assert_eq!(
        extra_directories(None, true, &[], |_| false),
        Vec::<PathBuf>::new(),
        "Homebrew's are checked too"
    );
}

/// `$SHELL` when it reads `-l -c` the POSIX way; zsh on a Mac and `/bin/sh`
/// elsewhere when it does not, or is not set.
// Unix paths: `/usr/bin` is not absolute on Windows.
#[cfg(unix)]
#[test]
fn the_login_shell_is_the_users_when_it_is_posix() {
    use std::ffi::OsStr;
    let shell = |value: Option<&str>, macos| login_shell_program(value.map(OsStr::new), macos);
    assert_eq!(shell(Some("/bin/bash"), true), PathBuf::from("/bin/bash"));
    assert_eq!(
        shell(Some("/opt/homebrew/bin/zsh"), false),
        PathBuf::from("/opt/homebrew/bin/zsh")
    );
    assert_eq!(
        shell(Some("/opt/homebrew/bin/fish"), true),
        PathBuf::from("/bin/zsh")
    );
    assert_eq!(shell(Some("/usr/bin/nu"), false), PathBuf::from("/bin/sh"));
    assert_eq!(shell(None, true), PathBuf::from("/bin/zsh"));
    assert_eq!(shell(Some(""), false), PathBuf::from("/bin/sh"));
    assert_eq!(
        shell(Some("zsh"), true),
        PathBuf::from("/bin/zsh"),
        "a bare name would itself need a PATH"
    );
}

/// **A child gets the effective `PATH`**, and a `PATH` the launch sets after
/// it still wins.
#[test]
fn a_child_command_is_given_the_effective_path() {
    let path_of = |command: &std::process::Command| {
        command
            .get_envs()
            .find(|(name, _)| *name == "PATH")
            .and_then(|(_, value)| value.map(|value| value.to_owned()))
    };
    let mut command = std::process::Command::new("koru-not-run");
    with_effective_path(&mut command);
    let effective = effective_path();
    if effective.is_empty() {
        assert_eq!(path_of(&command), None, "nothing to search, nothing set");
    } else {
        assert_eq!(path_of(&command), Some(effective));
    }
    command.env("PATH", "/a-profile-path");
    assert_eq!(path_of(&command), Some("/a-profile-path".into()));
}

/// **The real login shell, on this machine.** Not run by default: it runs
/// the person's profile, which a test must not depend on. Run it with
/// `cargo test -p koru-session-profiles -- --ignored login_shell` on a Mac
/// or Linux machine to see what Koru will add.
#[cfg(all(unix, not(any(target_os = "android", target_os = "ios"))))]
#[test]
#[ignore = "runs the login shell of whoever runs the test"]
fn the_login_shell_says_its_path() {
    let value = capture_login_path().expect("the login shell printed its PATH");
    let entries: Vec<_> = std::env::split_paths(&value).collect();
    eprintln!("{entries:#?}");
    assert!(entries.iter().any(|entry| entry == Path::new("/usr/bin")));
}
