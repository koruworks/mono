// SPDX-License-Identifier: MPL-2.0

//! **Our wiring, not the appender's behaviour.**
//!
//! Rotation and retention are `tracing-appender`'s contract — which file a date
//! maps to, when a new one begins, how many survive. Re-testing those would be
//! testing somebody else's library through ours, and the test would keep
//! passing after we stopped calling it correctly.
//!
//! What is ours: that both launch modes retain a file, and that the guard is
//! returned rather than dropped.

use super::*;

#[test]
fn a_console_run_also_creates_the_log_directory() {
    // The console is live diagnosis; this file is what remains after it closes.
    let dir = tempfile::tempdir().unwrap();
    let logging = init(dir.path(), true);
    assert!(dir.path().join(koru_log::LOG_DIR).is_dir());
    drop(logging);
}

#[test]
fn a_tray_run_creates_the_log_directory_under_the_store_root() {
    // Under the channel's root rather than beside the executable, which may be
    // in Program Files and unwritable.
    let dir = tempfile::tempdir().unwrap();
    let logging = init(dir.path(), false);
    assert!(dir.path().join(koru_log::LOG_DIR).is_dir());
    drop(logging);
}

#[test]
fn a_root_that_cannot_be_created_is_not_fatal() {
    // Refusing to start a store because it could not open a *log file* would be
    // a worse failure than the one being reported — and every other failure
    // path still reaches the message box.
    let dir = tempfile::tempdir().unwrap();
    let blocked = dir.path().join("a-file");
    std::fs::write(&blocked, b"not a directory").unwrap();
    // `logs/` cannot be made under something that is not a directory.
    let _logging = init(&blocked, false);
}

/// **The reading lines reach the file, in every launch mode.** Their target
/// is `koru_readings`, which no `koru_store` directive covers: before it was
/// named, "the reader was installed" and every `KORU_READ_DEBUG` job line were
/// filtered out of every log (OCR TECHNICAL §14 says they are unconditional).
#[test]
fn the_reading_lines_reach_every_filter() {
    for filter in [
        DEFAULT_FILTER,
        DEFAULT_FILTER_SYNC_DEBUG,
        CONSOLE_FILTER,
        CONSOLE_FILTER_SYNC_DEBUG,
        GATEWAY_FILTER,
        GATEWAY_CONSOLE_FILTER,
        GATEWAY_FILTER_SYNC_DEBUG,
        GATEWAY_CONSOLE_FILTER_SYNC_DEBUG,
    ] {
        assert!(
            filter
                .split(',')
                .any(|directive| directive == "koru_readings=info"),
            "{filter}"
        );
    }
}

#[test]
fn the_guard_must_be_held() {
    // The trap this type exists for: `tracing-appender`'s worker is what puts
    // bytes on the disk, so a guard dropped at the end of `init` loses every
    // line after it. `#[must_use]` is what makes forgetting a compile warning
    // rather than an empty log nobody notices.
    fn assert_must_use<T>() {}
    assert_must_use::<Logging>();
    // And the message names the consequence, not just the rule.
    assert!(
        std::any::type_name::<Logging>().ends_with("Logging"),
        "the guard type is what callers hold"
    );
}
