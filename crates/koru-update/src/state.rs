// SPDX-License-Identifier: MPL-2.0

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::ReleaseChannel;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CachedPackage {
    pub build_number: u64,
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FailedBuild {
    pub build_number: u64,
    pub failed_at: String,
    pub reason: String,
}

/// Automatic checks are on unless somebody turned them off. A `state.json`
/// written before the preference moved in here has no key at all, and serde's
/// `bool` default is `false` — which would silently stop updating for every
/// install that already exists.
fn automatic_checks_default() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpdateState {
    /// **Retained so an existing `state.json` still parses, and read by
    /// nothing.** The channel stopped being a preference when channel
    /// switching became "download that channel's zip": `UpdateController`
    /// takes its channel from `ReleaseChannel::of_this_build()` and there is
    /// no code path left that can point it anywhere else. Deleting the field
    /// outright would be fine for serde but would throw away a real user's
    /// recorded answer in the same release that stopped honouring it, so it
    /// stays for one, then goes.
    #[serde(default)]
    pub selected_channel: ReleaseChannel,
    /// Whether the controller checks on its own. It lives here rather than in
    /// `koru-window`'s `DesktopPreferences` because that file is written by
    /// `koru-workspace-host`, and a file with two writers cannot say which one lost —
    /// the agent and the app would each save a whole struct over the other's.
    /// This one has exactly one writer, is where `selected_channel` already
    /// lived, and is already at `UpdatePaths::state_file()`.
    #[serde(default = "automatic_checks_default")]
    pub automatic_checks: bool,
    #[serde(default)]
    pub last_check_at: Option<String>,
    #[serde(default)]
    pub last_seen_build: Option<u64>,
    #[serde(default)]
    pub deferred_build: Option<u64>,
    #[serde(default)]
    pub failed_build: Option<FailedBuild>,
    #[serde(default)]
    pub cached_verified_package: Option<CachedPackage>,
    #[serde(default)]
    pub etags: Vec<(String, String)>,
    /// The last manifest a check actually received, kept beside the ETag that
    /// would suppress the next one.
    ///
    /// **A 304 means the manifest has not changed, not that this build is up
    /// to date**, and without a copy of it there is nothing to answer a 304
    /// with. Losing that distinction cost a real update: the install was
    /// abandoned, the status became `Failed`, the next check sent the stored
    /// ETag, the origin answered 304, and the controller concluded *up to
    /// date* — with `last_seen_build` at 40, a verified 40 zip on disk, and
    /// the installed build at 39. Nothing the person could press said
    /// otherwise, because a manual check sends the same ETag.
    #[serde(default)]
    pub last_manifest: Option<crate::UpdateManifest>,
    /// The UTC day ([`crate::site::utc_day`]) of the last check `www.koru.do`
    /// answered with `first_today=1`. Only a day number: nothing that names
    /// the install, and no time of day.
    #[serde(default)]
    pub site_counted_day: Option<u64>,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            selected_channel: ReleaseChannel::default(),
            automatic_checks: automatic_checks_default(),
            last_check_at: None,
            last_seen_build: None,
            deferred_build: None,
            failed_build: None,
            cached_verified_package: None,
            etags: Vec::new(),
            last_manifest: None,
            site_counted_day: None,
        }
    }
}

impl UpdateState {
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let Some(parent) = path.parent() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "update state has no parent directory",
            ));
        };
        fs::create_dir_all(parent)?;
        let part = path.with_extension(format!("json.{}.part", std::process::id()));
        let mut file = fs::File::create(&part)?;
        serde_json::to_writer_pretty(&mut file, self).map_err(std::io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        if path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(part, path)
    }

    pub fn etag(&self, key: &str) -> Option<&str> {
        self.etags
            .iter()
            .find(|(saved, _)| saved == key)
            .map(|(_, value)| value.as_str())
    }

    pub fn set_etag(&mut self, key: String, value: String) {
        match self.etags.iter_mut().find(|(saved, _)| saved == &key) {
            Some(entry) => entry.1 = value,
            None => self.etags.push((key, value)),
        }
    }

    /// ETags are only useful while this process still has the corresponding
    /// manifest in memory. Carrying them across a restart could turn a 304
    /// response into a false "up to date" result.
    pub fn clear_etags(&mut self) {
        self.etags.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_state_is_ignored_and_a_valid_state_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, b"not json").unwrap();
        assert_eq!(UpdateState::load(&path), UpdateState::default());
        let mut state = UpdateState::default();
        state.last_seen_build = Some(44);
        state.set_etag("stable".into(), "abc".into());
        state.save(&path).unwrap();
        assert_eq!(UpdateState::load(&path), state);
    }

    #[test]
    fn a_state_file_written_before_the_key_existed_still_updates_automatically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, br#"{"last_seen_build":44}"#).unwrap();
        assert!(UpdateState::load(&path).automatic_checks);
    }
}
