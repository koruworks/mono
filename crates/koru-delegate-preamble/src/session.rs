// SPDX-License-Identifier: MPL-2.0

//! Common Koru session orientation, separate from artifact-run instructions.
//! The host injects the override path from its root-scoped settings. This leaf
//! never discovers configuration, creates a file, or judges an edit's meaning.

use std::path::{Path, PathBuf};

use crate::{Fault, Resolved, decode, read_bytes, resolve_candidates};

pub const RELATIVE_PATH: &str = "docs/prompts/session.md";
pub const SOURCE_PATH: &str = "crates/koru-delegate-preamble/session.md";
pub const BUNDLED: &str = include_str!("../session.md");

/// Host-owned locations, passed to a worker without reading on the caller's
/// thread. The default is entirely in memory, including in test hosts.
#[derive(Clone, Default)]
pub struct Configuration(Option<(PathBuf, Option<PathBuf>)>);

impl Configuration {
    pub fn files(installation: PathBuf, override_file: Option<PathBuf>) -> Self {
        Self(Some((installation, override_file)))
    }

    /// Call only when capturing a new input/session, on its worker thread.
    pub fn resolve(&self) -> Resolved {
        match &self.0 {
            Some((installation, override_file)) => resolve(installation, override_file.as_deref()),
            None => bundled(),
        }
    }
}

/// A default snapshot for a host with no configured file source. No disk read.
pub fn bundled() -> Resolved {
    crate::bundled(BUNDLED, None)
}

/// Snapshot the shared instructions for a new session, off the UI/actor thread.
/// The user's configuration override survives replacement of installed files.
/// Missing copies continue the search; an invalid existing copy returns the
/// compiled default with a content-free notice, never another editable copy.
pub fn resolve(installation_dir: &Path, override_file: Option<&Path>) -> Resolved {
    resolve_with(
        installation_dir,
        override_file,
        repository_file().as_deref(),
        |path| decode(read_bytes(path)?),
    )
}

fn resolve_with(
    installation_dir: &Path,
    override_file: Option<&Path>,
    repository_file: Option<&Path>,
    read: impl FnMut(&Path) -> Result<String, Fault>,
) -> Resolved {
    resolve_candidates(
        [
            override_file.map(Path::to_path_buf),
            Some(installation_dir.join(RELATIVE_PATH)),
            repository_file.map(Path::to_path_buf),
        ]
        .into_iter()
        .flatten(),
        BUNDLED,
        read,
    )
}

/// Compose a snapshot with the caller's truthful mode and runtime facts.
/// No substitutions or trimming are applied to the person's instructions.
/// Role, Knowledge, history and task remain owned by their existing renderer.
pub fn compose(common: &str, mode: &str, facts: &str) -> String {
    let mut text = common.to_owned();
    for section in [mode, facts] {
        if !section.is_empty() {
            text.push_str("\n\n");
            text.push_str(section);
        }
    }
    text
}

#[cfg(debug_assertions)]
fn repository_file() -> Option<PathBuf> {
    Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("session.md"))
}

#[cfg(not(debug_assertions))]
fn repository_file() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests;
