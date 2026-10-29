// SPDX-License-Identifier: MPL-2.0

//! What a person said no to, so a card they closed does not come back every
//! morning.
//!
//! One file, one writer (the Agent), beside the Agent's other state. It holds
//! only the version each dismissal was about: a newer release than the one
//! dismissed is news again, exactly as `koru-update`'s `deferred_build` works.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// CLI id → the latest version that was on the card when it was closed.
    #[serde(default)]
    pub dismissed: BTreeMap<String, String>,
}

impl State {
    /// A missing or unreadable file is an empty state, never a refusal: the
    /// worst outcome is a card shown again.
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Written whole and renamed into place, so a crash mid-write leaves the
    /// previous file rather than half of a new one.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let part = path.with_extension("json.part");
        {
            let mut file = fs::File::create(&part)?;
            file.write_all(&bytes)?;
            file.flush()?;
        }
        fs::rename(&part, path)
    }

    pub fn dismissed(&self, id: &str) -> Option<&str> {
        self.dismissed.get(id).map(String::as_str)
    }
}
