// SPDX-License-Identifier: MPL-2.0

//! **The shipped binary answers its command line**, before it touches a root,
//! a pipe or a file.
//!
//! Small on purpose. It is also why `cargo test --workspace` builds
//! `koru-read-worker` as an executable: cargo builds a package's binaries only
//! for that package's integration tests, and the Store's privacy gate
//! (`apps/koru-store-server/tests/reading_privacy.rs`) reads through this
//! executable, beside the Store's own.

use std::process::Command;

fn worker() -> Command {
    Command::new(env!("CARGO_BIN_EXE_koru-read-worker"))
}

#[test]
fn version_prints_and_exits() {
    let out = worker().arg("--version").output().expect("run the worker");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.starts_with("koru-read-worker ") && text.contains(env!("CARGO_PKG_VERSION")),
        "{text}"
    );
}

#[test]
fn an_argument_that_is_not_understood_is_not_run() {
    let out = worker().arg("--rot").output().expect("run the worker");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("--rot"), "the token is named: {text}");
    assert!(
        text.contains("--root"),
        "the nearest flag is suggested: {text}"
    );
}
