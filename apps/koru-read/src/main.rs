// SPDX-License-Identifier: MPL-2.0

//! Thin host for the read worker.
//!
//! **No `windows_subsystem = "windows"`.** `--help` and `--version` print, and a
//! supervised launch is spawned with `CREATE_NO_WINDOW` by the supervisor —
//! which knows whether anybody is watching, where this binary does not.

fn main() -> std::process::ExitCode {
    koru_read_worker::run()
}
