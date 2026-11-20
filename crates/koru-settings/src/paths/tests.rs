// SPDX-License-Identifier: MPL-2.0

use super::*;

/// **No test in this crate calls `discover`.** That is the point of the rooted
/// injection: `preferences.conf` could not be rooted before, so its
/// persistence was never covered by a test that did not write into the
/// developer's own `%APPDATA%`.
#[test]
fn a_rooted_layout_keeps_every_file_under_its_root() {
    let root = std::path::Path::new("/tmp/koru-test-root");
    let paths = SettingsPaths::under_root(root);
    for file in [
        paths.preferences_file(),
        paths.terminal_file(),
        paths.desktop_file(),
        paths.update_state_file(),
        paths.windows_file(),
    ] {
        assert!(
            file.starts_with(root),
            "{} escaped the root",
            file.display()
        );
    }
}

#[test]
fn two_roots_share_no_file() {
    let a = SettingsPaths::under_root(std::path::Path::new("/tmp/a"));
    let b = SettingsPaths::under_root(std::path::Path::new("/tmp/b"));
    assert_ne!(a.preferences_file(), b.preferences_file());
    assert_ne!(a, b);
}

#[test]
fn session_instructions_follow_the_injected_preferences_directory() {
    let paths = SettingsPaths::from_resolved_files(
        "/roaming/koru/preferences.conf".into(),
        "/other/terminal.conf".into(),
        "/local/desktop-v1.json".into(),
        "/cache/state.json".into(),
        "/state/windows.conf".into(),
    );
    assert_eq!(
        paths.session_instructions_file(),
        Path::new("/roaming/koru/prompts/session.md")
    );
    let ordinary = Path::new("/ordinary");
    let isolated =
        SettingsPaths::for_channel_root(Path::new("/isolated"), Some(ordinary), "test").unwrap();
    assert_eq!(
        isolated.session_instructions_file(),
        Path::new("/isolated/settings/prompts/session.md")
    );
    assert!(!isolated.session_instructions_file().starts_with(ordinary));
}

#[test]
fn each_file_has_the_name_its_current_owner_uses() {
    let paths = SettingsPaths::under_root(std::path::Path::new("/tmp/koru"));
    let name = |path: &std::path::Path| path.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(name(paths.preferences_file()), "preferences.conf");
    assert_eq!(name(paths.desktop_file()), "desktop-v1.json");
    assert_eq!(name(paths.windows_file()), "windows.conf");
    assert_eq!(name(paths.update_state_file()), "state.json");
    assert_eq!(
        name(paths.terminal_file()),
        "terminal.conf",
        "the only new file: one per section, so a Terminal write never \
         rewrites a byte of Global's"
    );
}

#[test]
fn a_staging_path_sits_beside_its_destination() {
    let destination = std::path::Path::new("/tmp/koru/preferences.conf");
    let staging = staging_path(destination);
    assert_eq!(staging.parent(), destination.parent());
    assert_ne!(staging, destination);
    let name = staging.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("preferences."));
    assert!(
        name.ends_with(".part"),
        "readers already ignore `.part` everywhere else in the tree"
    );
}

#[test]
fn a_staging_path_works_for_a_file_with_no_extension() {
    let staging = staging_path(std::path::Path::new("/tmp/koru/settings"));
    assert!(
        staging
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".part")
    );
}

#[test]
fn resolved_files_are_taken_exactly_as_given() {
    // The Gateway resolves production locations and hands them down, the way
    // the session host is given its already-resolved directory rather than a
    // product root it would have to do arithmetic on.
    let paths = SettingsPaths::from_resolved_files(
        "/a/preferences.conf".into(),
        "/b/terminal.conf".into(),
        "/c/desktop-v1.json".into(),
        "/d/state.json".into(),
        "/e/windows.conf".into(),
    );
    assert_eq!(
        paths.preferences_file(),
        std::path::Path::new("/a/preferences.conf")
    );
    assert_eq!(
        paths.desktop_file(),
        std::path::Path::new("/c/desktop-v1.json"),
        "the three production directories do not share a parent, which is why \
         this is four resolved paths and not one root"
    );
}

// ------------------------- where a delegated run's own folder is made (ADR-365)

/// **Nothing chosen means this machine's temporary directory** — `%TMP%` on
/// Windows, `/tmp` on Unix. Asserted against `std::env::temp_dir` rather than
/// against a spelled-out path, because the point is that Koru follows the
/// platform rather than freezing whatever it was the first time it looked.
#[test]
fn nothing_chosen_is_the_platforms_temporary_directory() {
    assert_eq!(delegate_temporary_base(None), std::env::temp_dir());
}

/// A blank or whitespace value reads as nothing chosen. A file left with `""`
/// must not send a run to the process's current directory.
#[test]
fn a_blank_choice_is_no_choice() {
    for blank in ["", "   ", "\t"] {
        assert_eq!(
            delegate_temporary_base(Some(blank)),
            std::env::temp_dir(),
            "{blank:?}"
        );
    }
}

/// A chosen folder is used as it stands, trimmed of the whitespace a paste
/// leaves behind.
#[test]
fn a_chosen_folder_is_the_base() {
    assert_eq!(
        delegate_temporary_base(Some("  D:/koru-runs  ")),
        std::path::PathBuf::from("D:/koru-runs")
    );
}

/// **It reads no disk.** A folder that does not exist still resolves — making
/// it is the host's business at the moment a run starts, and a resolver that
/// probed would be a resolver a window could not call.
#[test]
fn a_folder_that_is_not_there_still_resolves() {
    let missing = std::path::PathBuf::from("D:/no-such-folder-ac0f1f2e");
    assert_eq!(
        delegate_temporary_base(Some(&missing.to_string_lossy())),
        missing
    );
    assert!(!missing.exists(), "and nothing was created by asking");
}
