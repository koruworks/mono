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
