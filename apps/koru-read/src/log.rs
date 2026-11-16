// SPDX-License-Identifier: MPL-2.0

//! **Where the worker's output goes, and what it may never contain.**
//!
//! ```text
//! <store root>/logs/koru-read-worker.log.<date>
//! ```
//!
//! Beside the Store's own log, so a minute of a machine reads together.
//!
//! This process holds people's files. **No line carries a path, a file's name,
//! a title or a word from a page** (TECHNICAL §13, §14): the path a job names
//! is the blob store's object and is never logged; words travel only inside
//! `koru_read::PageText`, whose `Debug` is redacted; a page is described by its
//! number and its character count. Logging is news, not activity: a line per
//! job is `debug`, and there is never a line per page except for one that
//! exceeded its time budget.
//!
//! There is deliberately no `KORU_READ_DEBUG` read here. `RUST_LOG` turns a
//! target up; the debug variable belongs to the plan that gives it behaviour.

use std::path::Path;

pub use koru_log::Logging;

const DEFAULT_FILTER: &str = "warn,koru_read_worker=info,koru_read=info";
const CONSOLE_FILTER: &str = "warn,koru_read_worker=debug,koru_read=debug";

/// Start logging. A failure to open the file never decides whether the worker
/// starts — that is `koru-log`'s contract.
pub fn init(root: &Path, console: bool) -> Logging {
    if console {
        return koru_log::init_file_and_stdout(root, "koru-read-worker.log", CONSOLE_FILTER);
    }
    koru_log::init_file(root, "koru-read-worker.log", DEFAULT_FILTER)
}
