// SPDX-License-Identifier: MPL-2.0

//! What one spawn runs, decided without a process. The spawns themselves —
//! a sibling launched, contained, killed at its ceiling, released for an
//! update — are `tests/worker_process.rs`.

use super::*;

fn own_exe() -> PathBuf {
    // Built from components so the test means the same thing on every
    // platform's separator.
    ["install", "bin", "host-program"].iter().collect()
}

fn args(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

#[test]
fn an_own_role_is_this_executable_under_its_worker_token() {
    let (program, argv) = command_line(
        &Program::OwnRole,
        "gateway",
        &own_exe(),
        &args(&["--root", "r"]),
    )
    .unwrap();
    assert_eq!(program, own_exe());
    assert_eq!(argv, args(&["--worker", "gateway", "--root", "r"]));
}

#[test]
fn a_sibling_is_resolved_beside_this_executable_and_gets_no_worker_token() {
    let (program, argv) = command_line(
        &Program::Sibling("sibling-program".into()),
        "label",
        &own_exe(),
        &args(&["--root", "r", "--supervised"]),
    )
    .unwrap();
    let expected: PathBuf = ["install", "bin", "sibling-program"].iter().collect();
    assert_eq!(program, expected);
    // **Its own program, not a role of this one**: the role is a label here,
    // and a `--worker` in front would be an argument the sibling never asked
    // for.
    assert_eq!(argv, args(&["--root", "r", "--supervised"]));
}

#[test]
fn a_sibling_named_by_anything_but_a_plain_file_name_is_refused() {
    let mut absolute = std::env::temp_dir();
    absolute.push("elsewhere");
    for name in [
        OsString::from(""),
        OsString::from("."),
        OsString::from(".."),
        OsString::from("sub/sibling-program"),
        OsString::from("../sibling-program"),
        absolute.into_os_string(),
    ] {
        let refused = command_line(&Program::Sibling(name.clone()), "label", &own_exe(), &[]);
        assert!(refused.is_err(), "{name:?} was accepted as a sibling name");
    }
}

#[test]
fn a_sibling_needs_a_directory_to_be_beside() {
    // A bare relative name has an empty parent, which is a directory; only a
    // path with no parent at all — a root — has nowhere to look.
    let root: PathBuf = std::path::MAIN_SEPARATOR_STR.into();
    assert!(
        command_line(
            &Program::Sibling("sibling-program".into()),
            "label",
            &root,
            &[]
        )
        .is_err()
    );
}

#[test]
fn a_zero_memory_limit_is_refused_and_every_other_value_is_accepted() {
    assert!(check_memory_limit(Some(0)).is_err());
    assert!(check_memory_limit(None).is_ok());
    assert!(check_memory_limit(Some(1)).is_ok());
    assert!(check_memory_limit(Some(512 * 1024 * 1024)).is_ok());
}

#[test]
fn a_new_spec_is_an_unbounded_own_role_as_every_existing_role_expects() {
    let spec: WorkerSpec<()> =
        WorkerSpec::new("fake", |_| Err("never launched".into()), |_: &()| Ok(()));
    assert_eq!(spec.program, Program::OwnRole);
    assert_eq!(spec.memory_limit, None);
}

#[test]
fn the_memory_limit_is_enforced_exactly_where_there_is_a_job_object() {
    assert_eq!(MEMORY_LIMIT_ENFORCED, cfg!(windows));
}
