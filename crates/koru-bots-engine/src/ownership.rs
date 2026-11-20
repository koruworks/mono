// SPDX-License-Identifier: MPL-2.0

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::Error;

const MARKER: &str = "engine-owned";

/// The endpoint must already be exclusively bound. A failed claim prevents
/// the engine from serving. Retain the marker so older binaries refuse to
/// resume their retired Bot services on this root.
pub fn claim_root(root: &Path) -> Result<(), Error> {
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
