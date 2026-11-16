// SPDX-License-Identifier: MPL-2.0

//! Where a channel keeps its data.
//!
//! **A release channel is a hard boundary.** Stable, beta, nightly and a local
//! build never share a database, because they do not share a schema: a nightly
//! that has moved `workspace.db` forward would be opened by stable, refused,
//! and — if the refusal ever leaked — half-written. Separate directories make
//! that impossible rather than merely unlikely.
//!
//! The same logical workspace may exist in several of them at once. Those are
//! **replicas**: same immutable id, separate physical state, and one day they
//! will converge through ordinary sync. They must never open each other's
//! files.
//!
//! ## Five segments from a four-variant enum
//!
//! [`ReleaseChannel`] answers "what build is this", and returns `Internal` for
//! anything built locally by cargo. That is one segment too few: a debug build
//! and a `--release` build of the same tree are different binaries with
//! different behaviour, and sharing a store between them is exactly the
//! confusion this module removes. So `Internal` splits on
//! `cfg!(debug_assertions)`.
//!
//! This also retires the ad-hoc `cfg!(debug_assertions)` in
//! `crates/koru-workspace-host/src/startup.rs`, which decided the same question a
//! different way for `profiles.conf`.

use std::path::PathBuf;

use crate::ReleaseChannel;

/// The directory segment for a channel.
///
/// Stable ASCII tokens, and never a display string: this is a path component
/// and a store instance key, so it cannot be translated or prettified.
pub fn segment_of(channel: ReleaseChannel, debug: bool) -> &'static str {
    match channel {
        ReleaseChannel::Stable => "stable",
        ReleaseChannel::Beta => "beta",
        ReleaseChannel::Nightly => "nightly",
        ReleaseChannel::Internal if debug => "internal-debug",
        ReleaseChannel::Internal => "internal-release",
    }
}

/// This build's segment.
pub fn segment() -> &'static str {
    segment_of(ReleaseChannel::of_this_build(), cfg!(debug_assertions))
}

/// Every segment, so a person can be told where to look and a test can sweep
/// them.
pub const SEGMENTS: &[&str] = &[
    "stable",
    "beta",
    "nightly",
    "internal-debug",
    "internal-release",
];

/// `<local data>/Koru/<channel>/store`.
///
/// Machine-local by construction. **Nothing here belongs in a synced folder**:
/// a live SQLite database in OneDrive or Syncthing is two writers with no lock
/// between them, and the file-level conflict resolution those tools do is the
/// wrong shape for a database. Sharing a workspace across machines is what
/// export and, later, sync are for.
pub fn store_root() -> Option<PathBuf> {
    store_root_of(segment())
}

/// The store root of a **named** channel, not necessarily this one.
///
/// `store_root()` can only ever answer for the running binary, which is right
/// for the daemon and not enough for two other callers: a chooser listing which
/// channels have a store on this machine, and a test asserting that no two
/// channels resolve to one directory. That second one is why this is public —
/// "channels never share a database" was stated in three documents and asserted
/// nowhere, because there was no way to ask the question.
///
/// **This does not license opening another channel's store.** Resolving a path
/// is not permission to write to it; a build serves its own channel and no
/// other ([ADR-84](../../../docs/decisions/README.md)), and the daemon still derives
/// its own root from its own segment.
pub fn store_root_of(segment: &str) -> Option<PathBuf> {
    Some(store_root_under(&channel_root_of(segment)?))
}

/// The store's directory inside a channel root somebody named.
///
/// **The one place `--root` becomes a store root**, and the reason it exists is
/// that it was previously nowhere: an explicit `--root R` was handed to the
/// store unchanged, so a rooted store put `catalog.db`, `workspaces/` and
/// `logs/` loose in `R` while the agent put its device identity in `R/agent/`.
/// The two layouts disagreed — the default one has them as siblings under a
/// channel root, and the rooted one had the agent's directory *inside* the
/// store's, which is exactly what [`channel_root`] says must never happen: a
/// repair that clears the store would have taken the device identity with it.
///
/// So an app-level `--root R` is a **channel** root and mirrors the default
/// layout, `R/store` and `R/agent` beside each other. `koru-store-server --root` is
/// the exception and is unchanged: it is the daemon naming *its own* directory,
/// one level down, which is what every store suite passes it.
pub fn store_root_under(channel_root: &std::path::Path) -> PathBuf {
    channel_root.join("store")
}

/// Where Koru AI keeps its own data on this machine, given a channel root:
/// `ai.db`, and under `bots/` the folder each Bot works in when the person
/// chose none. `Koru\<segment>i`, beside `store/`, and `R/ai` under a
/// `--root R`.
///
/// The one place that join happens, so the gateway that opens the AI worker
/// there and the window that shows a new Bot's default folder before the Bot
/// exists cannot drift apart. Resolving the path creates nothing.
pub fn ai_root_under(channel_root: &std::path::Path) -> PathBuf {
    channel_root.join("ai")
}

/// Where the events journal lives, given a store root — the one rule, so the
/// gateway that opens it, the agent's tray row that reveals it and the
/// standalone events host cannot drift apart.
///
/// The sibling `events/` beside a conventional `store/` directory
/// (`Koru\<segment>\events` next to `Koru\<segment>\store`, and likewise for
/// a `--root R` stack's `R/store`); *inside* the root otherwise, so a test
/// harness that passes a bare temporary directory as the store root never
/// grows a sibling outside its own tree.
pub fn events_root_beside(store_root: &std::path::Path) -> PathBuf {
    match (store_root.file_name(), store_root.parent()) {
        (Some(name), Some(parent)) if name == "store" => parent.join("events"),
        _ => store_root.join("events"),
    }
}

/// Everything this channel keeps on this machine.
///
/// `%LOCALAPPDATA%\Koru\<segment>` on Windows,
/// `~/Library/Application Support/Koru/<segment>` on macOS (with a sticky
/// legacy rule — see [`channel_root_of`]), `$XDG_DATA_HOME/Koru/<segment>`
/// elsewhere. The store's directory is one *inside* it, and so is the agent's:
///
/// ```text
/// Koru/<segment>/          <- this
///   store/                 <- the store owns this and everything under it
///     workspaces/  catalog.db  projection.db  search.db  logs/  outbox/
///   agent/                 <- device.json, and the agent's own preferences
/// ```
///
/// **The two are siblings, deliberately.** The store owns its directory
/// completely — a repair that clears it must be able to, and a person who
/// deletes it must lose only store data. The agent's device identity is minted
/// once and never derived ([ADR-39](../../../docs/decisions/README.md)), so putting it
/// under the store's would make "clear the store" silently mean "become a
/// different device".
pub fn channel_root() -> Option<PathBuf> {
    channel_root_of(segment())
}

/// `<local data>/Koru` — the one directory every channel root lives under.
///
/// This is where a value **shared across installed channels** belongs: beside
/// the segment directories, under none of them. The relay preference is the
/// first such value (decision №7 of the iroh-relay roadmap). It deliberately
/// skips the macOS legacy probe below — a cross-channel value needs *one*
/// agreed place, and a new file starts in the preferred one.
pub fn shared_root() -> Option<PathBuf> {
    local_data_base().map(|base| base.join("Koru"))
}

/// The channel root of a **named** channel. See [`store_root_of`] for why the
/// `_of` half of each pair is public.
///
/// **On macOS an existing XDG root sticks.** ADR-116 moved the settings files
/// to `~/Library/Application Support`, but it names only `preferences.conf`
/// and `windows.conf` — the databases under this root are a separate decision,
/// because moving them is data migration. The rule here gives fresh Macs the
/// right place at the cheap moment (before any real data exists) with zero
/// migration code: a machine whose legacy `~/.local/share/Koru/<segment>`
/// already exists keeps answering it until the packaging phase ships a real
/// move. Three processes resolve this root; the probe is deterministic — the
/// preferred root wins the moment it exists, and only a build answering
/// *preferred* ever creates it, so a legacy machine cannot flip by accident.
pub fn channel_root_of(segment: &str) -> Option<PathBuf> {
    let preferred = local_data_base()?.join("Koru").join(segment);
    if cfg!(target_os = "macos")
        && let Some(legacy) = xdg_data_base().map(|base| base.join("Koru").join(segment))
    {
        let choice = pick_root(&preferred, &legacy, preferred.is_dir(), legacy.is_dir());
        return Some(choice.to_path_buf());
    }
    Some(preferred)
}

/// The whole rule, pure so it is testable without a filesystem: legacy only
/// when it exists and the preferred root does not.
fn pick_root<'a>(
    preferred: &'a std::path::Path,
    legacy: &'a std::path::Path,
    preferred_exists: bool,
    legacy_exists: bool,
) -> &'a std::path::Path {
    if !preferred_exists && legacy_exists {
        legacy
    } else {
        preferred
    }
}

fn local_data_base() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        // The macOS analogue of `%LOCALAPPDATA%` — where `koru_update` already
        // files, and where `koru-window`'s two settings resolvers now point.
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
    } else {
        xdg_data_base()
    }
}

/// `$XDG_DATA_HOME` or `~/.local/share` — the non-Windows default, and on
/// macOS the legacy location [`channel_root_of`] still honours.
fn xdg_data_base() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
}

/// Whether a path looks like it is inside a folder something else synchronises.
///
/// Advisory only — it cannot be enforced and must never refuse to start, since
/// a false positive would lock someone out of their own data. It exists so that
/// a corruption report has the one fact most likely to explain it.
pub fn looks_synced(path: &std::path::Path) -> Option<&'static str> {
    const SUSPECTS: &[(&str, &str)] = &[
        ("onedrive", "OneDrive"),
        ("dropbox", "Dropbox"),
        ("google drive", "Google Drive"),
        ("googledrive", "Google Drive"),
        ("icloud", "iCloud Drive"),
        ("nextcloud", "Nextcloud"),
        ("syncthing", "Syncthing"),
        ("pcloud", "pCloud"),
    ];
    let haystack = path.to_string_lossy().to_lowercase();
    SUSPECTS
        .iter()
        .find(|(needle, _)| haystack.contains(needle))
        .map(|(_, name)| *name)
}

/// Where the delegate worker keeps `runs.db`, given a store root — the same
/// one rule as [`events_root_beside`], so the worker that opens the ledger,
/// the diagnostics that read it with the worker stopped and the tray row that
/// reveals it cannot drift apart.
///
/// The sibling `delegate/` beside a conventional `store/` directory
/// (`Koru\<segment>\delegate` next to `Koru\<segment>\store`, and likewise
/// for a `--root R` stack's `R/store`); *inside* the root otherwise, so a
/// test harness that passes a bare temporary directory as the store root
/// never grows a sibling outside its own tree. The ledger is device-local
/// execution state and never syncs, which is why it lives beside the store
/// and not under it: a repair that clears the store must not decide what
/// this machine remembers about its running work, and clearing this
/// directory must not touch a document.
pub fn delegate_root_beside(store_root: &std::path::Path) -> PathBuf {
    match (store_root.file_name(), store_root.parent()) {
        (Some(name), Some(parent)) if name == "store" => parent.join("delegate"),
        _ => store_root.join("delegate"),
    }
}

/// Where the artifact store's database lives, given a store root — the one
/// rule, so the gateway worker that opens `artifacts.db` (D13) and the test
/// that asserts two roots hold two databases cannot drift apart.
///
/// The sibling `artifacts/` beside a conventional `store/` directory
/// (`Koru\<segment>\artifacts` next to `Koru\<segment>\store` and
/// `Koru\<segment>\ai`, and likewise for a `--root R` stack's `R/store`);
/// *inside* the root otherwise, for [`events_root_beside`]'s reason: a test
/// harness that passes a bare temporary directory never grows a sibling
/// outside its own tree. The directory is created by the worker that opens
/// the database, never by resolving the path.
pub fn artifacts_root_beside(store_root: &std::path::Path) -> PathBuf {
    match (store_root.file_name(), store_root.parent()) {
        (Some(name), Some(parent)) if name == "store" => parent.join("artifacts"),
        _ => store_root.join("artifacts"),
    }
}

#[cfg(test)]
mod tests;
