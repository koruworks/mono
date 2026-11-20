// SPDX-License-Identifier: MPL-2.0

//! File locations for the AI context worker. No instruction bytes are read here.

use std::path::{Path, PathBuf};

pub(super) fn configure(
    worker: koru_ai_server::WorkerConfig,
    root: &Path,
) -> koru_ai_server::WorkerConfig {
    let Some(installation) = std::env::current_exe()
        .ok()
        .and_then(|exe| data_directory(&exe, cfg!(target_os = "macos")))
    else {
        return worker;
    };
    let override_file =
        super::settings::settings_paths(root).map(|paths| paths.session_instructions_file());
    worker.with_session_instructions(installation, override_file)
}

fn data_directory(executable: &Path, macos: bool) -> Option<PathBuf> {
    let directory = executable.parent()?;
    if macos && directory.file_name().is_some_and(|name| name == "MacOS") {
        Some(directory.parent()?.join("Resources"))
    } else {
        Some(directory.to_path_buf())
    }
}

#[cfg(test)]
mod tests;
