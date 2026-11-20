// SPDX-License-Identifier: MPL-2.0

//! Where the settings files are — resolved once, injected everywhere.
//!
//! **`preferences.conf` could not be rooted before this crate existed**, and
//! that is survey note 3. `koru_window::user_preferences_path` takes no
//! arguments and reads `APPDATA`/`HOME`/`XDG_CONFIG_HOME` directly, so both
//! the reader and the writer in `koru-workspace-host` resolved a real
//! per-developer path with no override and no setter. The consequence is
//! written down in that file: a test that exercised saving would write into
//! `%APPDATA%`, which is the developer's own settings, so the tests assert on
//! a pure comparison function instead and the persistence itself was never
//! covered.
//!
//! Here every path arrives through [`SettingsPaths`]. Production calls
//! [`SettingsPaths::discover`] once at the Gateway boundary; every test calls
//! [`SettingsPaths::under_root`] with a temp directory. Nothing else resolves
//! a location, so no test can reach a real preferences or local-app-data path
//! even by accident.

use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// The roaming file every Global setting that follows the person lives in.
pub const PREFERENCES_FILE: &str = "preferences.conf";
/// The roaming file the Terminal section lives in.
///
/// A second file rather than a prefix inside `preferences.conf`: the sections
/// are patched independently, and one file per section means a Terminal write
/// never rewrites a byte of Global's — which is the same argument per-field
/// patches make one level down.
pub const TERMINAL_FILE: &str = "terminal.conf";
/// The device-local JSON file the machine-shaped Global settings live in.
pub const DESKTOP_FILE: &str = "desktop-v1.json";
/// `koru-update`'s own state file. Read exactly once, to migrate one value.
pub const UPDATE_STATE_FILE: &str = "state.json";
/// The device-local geometry file. Read exactly once, to seed the Terminal
/// section from this machine's row.
pub const WINDOWS_FILE: &str = "windows.conf";

/// Every file the authority may touch, already resolved.
///
/// Deliberately a struct of final paths rather than a root plus arithmetic:
/// the four locations do not share a parent in production — roaming config,
/// `ProjectDirs`' local config, local state, and the channel's update
/// directory are four different places — so a single root would be a lie
/// everywhere except under a test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsPaths {
    preferences: PathBuf,
    terminal: PathBuf,
    desktop: PathBuf,
    update_state: PathBuf,
    windows: PathBuf,
}

impl SettingsPaths {
    /// Build from four already-resolved locations.
    pub fn from_resolved_files(
        preferences: PathBuf,
        terminal: PathBuf,
        desktop: PathBuf,
        update_state: PathBuf,
        windows: PathBuf,
    ) -> Self {
        Self {
            preferences,
            terminal,
            desktop,
            update_state,
            windows,
        }
    }

    /// The production layout, reproducing what each current owner resolves.
    ///
    /// **This is the only function in the crate that reads the environment**,
    /// it is called once at the Gateway boundary, and no test calls it. The
    /// roaming half matches `koru_window::user_preferences_path`; the
    /// `desktop-v1.json` half matches `DesktopPreferences::path`, which uses
    /// `ProjectDirs::from("do", "Koru", "Koru")` and therefore does *not* sit
    /// beside the geometry file; the geometry half matches
    /// `koru_window::window_geometry_path`. Three different directories, kept
    /// exactly as they are because moving somebody's file is not a refactor.
    /// `channel_segment` qualifies only the update-state locator: the
    /// preferences and geometry files are shared by every channel today, and
    /// the update directory is not.
    pub fn discover(channel_segment: &str) -> Option<Self> {
        let roaming = roaming_dir()?;
        let desktop = directories::ProjectDirs::from("do", "Koru", "Koru")
            .map(|dirs| dirs.config_local_dir().join(DESKTOP_FILE))?;
        Some(Self {
            preferences: roaming.join(PREFERENCES_FILE),
            terminal: roaming.join(TERMINAL_FILE),
            desktop,
            update_state: update_cache_dir()?
                .join("updates")
                .join(channel_segment)
                .join(UPDATE_STATE_FILE),
            windows: local_state_dir()?.join(WINDOWS_FILE),
        })
    }

    /// Every file under one directory.
    ///
    /// The rooted shape a `--root` run and every test uses. A rooted authority
    /// keeps its own settings and can never read or replace the ordinary
    /// channel's — the same guarantee the session host's rooted registries
    /// give.
    pub fn under_root(root: &Path) -> Self {
        Self {
            preferences: root.join(PREFERENCES_FILE),
            terminal: root.join(TERMINAL_FILE),
            desktop: root.join(DESKTOP_FILE),
            update_state: root.join("updates").join(UPDATE_STATE_FILE),
            windows: root.join(WINDOWS_FILE),
        }
    }

    /// **Which of the two layouts a stack rooted at `channel_root` reads.**
    ///
    /// `ordinary` is the channel root an installation would use — the caller
    /// supplies it, and the segment, because this crate does not know what a
    /// channel is. When the two agree this is an installation and the three
    /// production locations are resolved; when they differ, `--root` named
    /// another one and the settings live under it, where a rehearsal can
    /// never read or replace the real ones.
    ///
    /// One function because **two processes ask it**. The gateway opens the
    /// authority that writes; the delegate worker reads the one device-local
    /// value it needs to place a run's folder. Two spellings of this rule
    /// would be a rooted worker quietly reading an installation's preferences.
    pub fn for_channel_root(
        channel_root: &Path,
        ordinary: Option<&Path>,
        segment: &str,
    ) -> Option<Self> {
        match ordinary == Some(channel_root) {
            true => Self::discover(segment),
            false => Some(Self::under_root(&channel_root.join("settings"))),
        }
    }

    pub fn preferences_file(&self) -> &Path {
        &self.preferences
    }

    /// The shared session instructions override, beside roaming preferences.
    /// This is path arithmetic only: it neither reads nor creates a file, and
    /// an explicit root keeps its override inside that root's settings folder.
    pub fn session_instructions_file(&self) -> PathBuf {
        self.preferences
            .with_file_name("prompts")
            .join("session.md")
    }

    pub fn terminal_file(&self) -> &Path {
        &self.terminal
    }

    pub fn desktop_file(&self) -> &Path {
        &self.desktop
    }

    /// `koru-update`'s state file. **Read-only, and read for one key.**
    ///
    /// The authority never writes here: `automatic_checks` moved out into
    /// `desktop-v1.json`, and everything else in that file stays
    /// `koru-update`'s. See `crate::storage::migrate_automatic_checks`.
    pub fn update_state_file(&self) -> &Path {
        &self.update_state
    }

    /// The geometry file. **Read-only**, and read once, to seed the Terminal
    /// section from this machine's row.
    pub fn windows_file(&self) -> &Path {
        &self.windows
    }
}

/// `%APPDATA%\koru` / `~/Library/Application Support/koru` / `$XDG_CONFIG_HOME/koru`.
fn roaming_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }?;
    Some(base.join("koru"))
}

/// `%LOCALAPPDATA%\koru` / `~/Library/Application Support/koru` / `$XDG_STATE_HOME/koru`.
fn local_state_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("state"))
            })
    }?;
    Some(base.join("koru"))
}

/// Where `koru-update` keeps its cache, and therefore its `state.json`.
///
/// **A deliberate second copy of somebody else's layout, and a temporary one.**
/// It exists to locate one value for one migration read — `automatic_checks`,
/// on the way out of that file — and the authority never writes there. Linking
/// `koru-update` to borrow `UpdatePaths::discover` would pull `zip`,
/// `minisign-verify`, `sha2` and an HTTP client into the settings authority
/// and through it into the Gateway, which is a great deal of weight for one
/// path. This locator can be deleted once no install predates the move; until
/// then, if it is wrong the only consequence is that somebody who had turned
/// automatic checks off gets `koru-update`'s own default of `true` back once.
fn update_cache_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA").map(|local| PathBuf::from(local).join("koru"));
    }
    directories::BaseDirs::new().map(|base| base.cache_dir().join("koru"))
}

/// A staging path beside a destination, for a write that publishes by rename.
///
/// The name carries the process id so two hosts staging at once cannot write
/// the same temporary file, and readers ignore anything ending `.part` —
/// the convention `koru-session-paths`, `koru-docstore` and `koru-update`
/// already share.
pub fn staging_path(destination: &Path) -> PathBuf {
    let suffix = match destination.extension() {
        Some(extension) => format!(
            "{}.{}.part",
            extension.to_string_lossy(),
            std::process::id()
        ),
        None => format!("{}.part", std::process::id()),
    };
    destination.with_extension(suffix)
}

/// **Where a delegated run's own folder is made**, resolved once (ADR-365).
///
/// `configured` is [`crate::GlobalSettings::delegate_temporary_root`] — the
/// folder a person chose. Nothing chosen, or a blank one, means the platform's
/// temporary directory: `%TMP%` on Windows, `/tmp` on Unix, which is what
/// [`std::env::temp_dir`] answers and what the maintainers asked for.
///
/// **The one place the default is spelled.** The modal that shows the path and
/// the host that makes the folder are in two processes; a second spelling of
/// the fallback would be two answers to "where did my run's work go", and the
/// person would only find out which was right by looking in both.
///
/// It reads the environment and **not the disk**, so a window may ask it: no
/// file is opened, nothing is created, and the answer is whatever this machine
/// currently calls its temporary directory. Whether the directory can be
/// written to is a different question, asked where the folder is chosen.
pub fn delegate_temporary_base(configured: Option<&str>) -> PathBuf {
    configured
        .map(str::trim)
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// The folder inside a base that Koru's own run directories go in.
///
/// A run's workspace is `<base>/<RUNS_FOLDER>/<run>/workspace`. The middle
/// segment exists because the base is usually a directory Koru does not own —
/// a person's `%TMP%` has a hundred other things in it — and one named folder
/// is the difference between a tidy tree somebody can find and delete, and a
/// scatter of bare identifiers nobody can attribute.
pub const RUNS_FOLDER: &str = "koru-runs";
