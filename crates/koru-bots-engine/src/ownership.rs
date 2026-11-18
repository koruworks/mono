// SPDX-License-Identifier: MPL-2.0

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::Error;

const MARKER: &str = "engine-owned";

/// Called by legacy compositions before opening their Bot services. An
/// unreadable marker is not permission to fall back. Its presence, even if
/// truncated by a crash, permanently retires the legacy path on this root.
pub fn legacy_allowed(root: &Path) -> Result<bool, Error> {
    if crate::ENABLED {
        return Ok(false);
    }
    match fs::symlink_metadata(root.join("bots").join(MARKER)) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(_) => Err(Error::StorageUnavailable),
    }
}

/// The endpoint must already be exclusively bound. A failed claim prevents
/// the engine from serving. No code removes this marker on stop or rollback.
pub fn claim_root(root: &Path) -> Result<(), Error> {
    if !crate::ENABLED {
        return Err(Error::EngineDisabled);
    }
    let folder = root.join("bots");
    fs::create_dir_all(&folder).map_err(|_| Error::StorageUnavailable)?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(folder.join(MARKER))
    {
        Ok(mut file) => {
            file.write_all(b"koru-bots-engine 1\n")
                .map_err(|_| Error::StorageUnavailable)?;
            file.sync_all().map_err(|_| Error::StorageUnavailable)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(_) => Err(Error::StorageUnavailable),
    }
}
