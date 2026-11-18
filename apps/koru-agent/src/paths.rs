// SPDX-License-Identifier: MPL-2.0

//! Where the agent keeps things, and what an explicit `--root` refuses.
//!
//! ## `--root` isolates everything, or the suite edits the developer's machine
//!
//! An explicit root is how the gate runs several agents at once. Every
//! machine-wide thing the agent touches has to be redirected under it or turned
//! off — otherwise a `cargo test` run rewrites the global shortcut, adds a
//! Run-key entry and puts icons in the notification area of whoever ran it.
//!
//! | under an explicit `--root` | |
//! | --- | --- |
//! | the agent's own endpoint | a digest of the root appended, by `koru-ipc` |
//! | the store it ensures | the same root passed through |
//! | `device.json` | `<root>/agent/`, never `%LOCALAPPDATA%` |
//! | `search.conf` | `<root>/agent/`, never `%LOCALAPPDATA%` |
//! | reminder ledger/native namespace | ledger under `<root>/agent/`; OS ids use only a stable root hash |
//! | `DesktopPreferences` | `<root>/agent/`, never `%APPDATA%` |
//! | session profiles and attachments | `<root>/agent/sessions/` |
//! | global input | **not registered unless interactive `--tray` is explicitly requested** |
//! | the tray icon | **not created unless `--tray` is explicitly requested** |
//! | start-on-login | **refused** |
//!
//! Global chords and the Run entry are machine-wide. A headless test root never
//! registers either. The interactive `--tray` opt-in also enables global input;
//! `RegisterHotKey` safely refuses chords another process owns. Start-at-login
//! remains refused because its one Run value cannot be root-isolated. Tray icons
//! can coexist, but a notification area filling with icons during a test run is
//! its own answer, so a rooted agent still needs the explicit opt-in.
//!
//! **This is a decision about a *root*, not about `cfg(test)`.** A developer who
//! passes `--root` by hand gets the same isolation, which is what makes the
//! escape hatch usable rather than a way to half-clobber your own machine.

use std::path::{Path, PathBuf};

mod session;

pub use session::{
    AtomicPath, CleanupAuthority, CleanupPredicate, CleanupReport, SessionPaths,
    VerifiedAttachmentExtension,
};

/// Where this agent lives, and therefore what it may touch.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Paths {
    root: PathBuf,
    /// Whether the root was named on the command line, rather than resolved
    /// from the channel. Everything below turns on this.
    explicit: bool,
}

impl Paths {
    /// The channel's own root — the ordinary case.
    pub fn for_channel(root: PathBuf) -> Self {
        Self {
            root,
            explicit: false,
        }
    }

    /// A root somebody named. See the module note for what this refuses.
    pub fn for_root(root: PathBuf) -> Self {
        Self {
            root,
            explicit: true,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `Some` only when a root was named, which is exactly what
    /// `Endpoint::for_service_root` and the gateway worker host want:
    /// `None` means "the channel's own", which the store works out itself.
    pub fn explicit_root(&self) -> Option<&Path> {
        self.explicit.then_some(self.root.as_path())
    }

    /// `<root>/agent/` — this agent's own directory.
    ///
    /// **Beside the store's, never inside it.** On the ordinary launch the root
    /// is the *channel's* (`Koru/<channel>/`), so this is
    /// `Koru/<channel>/agent/` and the store's is `Koru/<channel>/store/`. The
    /// store owns its directory completely — a repair that clears it must be
    /// able to — and a device identity is minted once and never derived, so
    /// keeping it under the store's would make "clear the store" silently mean
    /// "become a different device".
    ///
    /// Under an explicit `--root` the two do share a directory, and that is
    /// what a gate wants: one temp directory per instance, thrown away whole.
    pub fn agent_dir(&self) -> PathBuf {
        self.root.join("agent")
    }

    /// The complete resolved filesystem contract for terminal sessions.
    ///
    /// Runtime libraries receive this value (or one of its resolved paths);
    /// they never receive a root and derive a second layout from it.
    pub fn session_paths(&self) -> SessionPaths {
        SessionPaths::from_resolved_directory(self.agent_dir().join("sessions"))
    }

    /// The Store directory paired with this Agent.
    pub fn store_dir(&self) -> PathBuf {
        koru_channel::store_root_under(&self.root)
    }

    /// The device identity. Under the root when one was named, so a gate cannot
    /// mint a device id into a developer's real profile.
    pub fn device_file(&self) -> PathBuf {
        self.agent_dir().join("device.json")
    }

    /// Rebuildable, content-free native reminder registrations for this device.
    pub fn reminder_ledger_file(&self) -> PathBuf {
        self.agent_dir().join("reminder-delivery-v1.json")
    }

    /// The user-selected reminder delivery choice for this Agent only.
    ///
    /// This is separate from the rebuildable ledger: losing a cache must not
    /// silently reverse a person's choice. It is also outside the Store, so it
    /// neither synchronizes nor addresses another device.
    pub fn reminder_preferences_file(&self) -> PathBuf {
        self.agent_dir().join("reminder-preferences-v1.json")
    }

    /// The authoritative public identity and signed trust database.
    pub fn identity_db(&self) -> PathBuf {
        self.agent_dir().join("agent.db")
    }

    /// Durable cross-process Profile deletion and cleanup journal.
    pub fn profile_lifecycle_db(&self) -> PathBuf {
        self.agent_dir().join("profile-lifecycle.db")
    }

    /// This device's remembered per-Profile Cloud sync choice, and what it
    /// last learned about each Workspace's hosted row. Content-free, and
    /// re-derivable: losing it costs a re-activation, never content.
    pub fn cloud_sync_state(&self) -> PathBuf {
        self.agent_dir().join("cloud-sync.json")
    }

    /// Catalog units Koru Cloud refused because they name a Profile the
    /// account does not hold, per account, so a restart does not ask again.
    /// Content-free (unit ids and version vectors) and re-derivable: losing
    /// it costs one request per refused unit.
    pub fn cloud_refused_units(&self) -> PathBuf {
        self.agent_dir().join("cloud-refused-units.json")
    }

    /// DPAPI-protected private key blobs. Never part of Store repair.
    pub fn secrets_dir(&self) -> PathBuf {
        self.agent_dir().join("secrets")
    }

    /// Rolling, privacy-safe search routing decisions from the agent.
    pub fn routing_log_file(&self) -> PathBuf {
        self.agent_dir().join("routing.log")
    }

    /// Stable, root-relative location suitable for status and bug reports.
    ///
    /// A resolved path contains the person's channel root, so it must not be
    /// printed by diagnostics that are routinely pasted into reports.
    pub fn routing_log_label(&self) -> &'static str {
        "agent/routing.log"
    }

    /// The machine-local window geometry shared with Koru itself.
    ///
    /// An explicit root redirects it with every other agent-owned file so a
    /// test or second stack cannot move the real global-search window.
    pub fn window_geometry_file(&self) -> Option<PathBuf> {
        koru_window::window_geometry_path(self.explicit_root())
    }

    /// What global search reopens on — the profile choice, machine-local and
    /// shared with Koru itself, in the directory `windows.conf` lives in.
    ///
    /// Redirected under an explicit root for the same reason the geometry is:
    /// a gate that changed which profile a person's search opens in would be a
    /// test that edited the machine it ran on.
    pub fn search_state_file(&self) -> Option<PathBuf> {
        if self.explicit {
            Some(self.agent_dir().join("search.conf"))
        } else {
            koru_window::search_state_path()
        }
    }

    /// **What somebody agreed to before Koru downloads a speech model or opens
    /// a microphone** — machine-local, shared with Koru itself, in the
    /// directory `windows.conf` lives in.
    ///
    /// Redirected under an explicit root for the third time and the same
    /// reason: a gate that recorded consent into a person's real record would
    /// be a test that agreed to something on their behalf, and one that read it
    /// would pass or fail depending on whether the machine it ran on had
    /// already said yes.
    pub fn voice_consent_file(&self) -> Option<PathBuf> {
        if self.explicit {
            Some(self.agent_dir().join(koru_voice_consent::CONSENT_FILE))
        } else {
            koru_window::voice_consent_path()
        }
    }

    /// Machine-local desktop behaviour.
    ///
    /// `None` means "wherever `DesktopPreferences` puts it" — `%APPDATA%`, the
    /// one a person's real Koru reads. Under an explicit root it is redirected,
    /// because a test that rewrote the global shortcut would be a test that
    /// broke the machine it ran on.
    pub fn preferences_file(&self) -> Option<PathBuf> {
        self.explicit
            .then(|| self.agent_dir().join("desktop-v1.json"))
    }

    /// The machine-local directory containing window geometry and other
    /// settings that must not roam between devices.
    pub fn local_data_dir(&self) -> Option<PathBuf> {
        self.window_geometry_file()
            .and_then(|path| path.parent().map(Path::to_path_buf))
    }

    /// The roaming directory containing preferences shared with Koru.
    pub fn shared_data_dir(&self) -> Option<PathBuf> {
        let file = self
            .preferences_file()
            .or_else(koru_window::user_preferences_path)?;
        file.parent().map(Path::to_path_buf)
    }

    /// Whether this agent may take the default machine-wide integrations.
    ///
    /// The interactive root launcher can explicitly add its tray and global
    /// input without changing this answer; startup and other default ownership
    /// never follow it.
    pub fn owns_the_machine(&self) -> bool {
        !self.explicit
    }
}

#[cfg(test)]
mod tests;
