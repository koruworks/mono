// SPDX-License-Identifier: MPL-2.0

//! The settings authority, and the one startup side effect it cannot perform
//! itself.

use std::sync::Arc;

use koru_api_gateway_core::GatewayState;

/// **Hand the settings authority down before anything is served.**
///
/// A rooted run keeps its own settings under that root and can never read or
/// replace the ordinary channel's — the same guarantee the rooted Store and
/// session host give, and the reason `preferences.conf` needed a rooted
/// resolver at all (it had none before SP-6). An ordinary run resolves the
/// three production locations, which do not share a parent.
///
/// A host that cannot resolve them opens no authority, and `/v1/settings/*`
/// then answers `capability_unavailable` rather than writing somewhere guessed.
pub(super) fn install_authority(channel_root: &std::path::Path, state: &Arc<GatewayState>) {
    match settings_paths(channel_root) {
        Some(paths) => {
            let authority = koru_settings::SettingsAuthority::new(paths);
            // **And the one side effect the authority cannot perform itself.**
            // Windows names the silent Agent explicitly. Other platforms keep
            // their current product target until their handoff lands.
            if owns_machine_startup(channel_root, koru_channel::channel_root().as_deref()) {
                authority.set_start_on_login_port(Arc::new(StartOnLogin));
            }
            state.set_settings_authority(authority);
        }
        None => tracing::warn!(
            "no settings location could be resolved; the settings API will report itself unavailable"
        ),
    }
}

/// Where this gateway's settings authority reads and writes.
///
/// **A rooted run gets rooted settings.** `channel_root` is the ordinary
/// channel root unless `--root` named another one, so comparing the two is how
/// this tells a rehearsal or a test from an installation — and a rooted
/// gateway must never be able to read or replace the ordinary channel's
/// preferences, which is exactly what `preferences.conf` had no way to
/// guarantee before this authority existed.
///
/// **The rule itself is `koru-settings`'** now, because the delegate worker
/// asks it too: it reads the one device-local value that says where a run's
/// own folder is made (ADR-365), and a second spelling of "rooted or not"
/// would be a rehearsal quietly reading an installation's preferences. What
/// stays here is the part that is the composition's to know — which channel
/// this is.
pub(super) fn settings_paths(
    channel_root: &std::path::Path,
) -> Option<koru_settings::SettingsPaths> {
    koru_settings::SettingsPaths::for_channel_root(
        channel_root,
        koru_channel::channel_root().as_deref(),
        koru_channel::segment(),
    )
}

fn owns_machine_startup(
    channel_root: &std::path::Path,
    ordinary: Option<&std::path::Path>,
) -> bool {
    ordinary == Some(channel_root)
}

/// **The OS side of `start_on_login`, performed by the host that can.**
///
/// The authority refuses the field outright without one of these, and that is
/// deliberate: a stored `true` the registry never got is a lie the person only
/// discovers at the next sign-in, so the side effect goes first and the value
/// commits only if it returned `Ok` — exactly what `DesktopPreferences` did
/// before the setting moved.
struct StartOnLogin;

impl koru_settings::StartOnLoginPort for StartOnLogin {
    fn set(&self, enabled: bool) -> Result<(), String> {
        let entry = koru_autostart::channel_entry(koru_autostart::KORU_ENTRY);
        if !enabled {
            return koru_autostart::clear(&entry).map_err(|error| error.to_string());
        }
        let executable = agent_executable()?;
        let command = startup_command(&executable);
        koru_autostart::set(&entry, &command).map_err(|error| error.to_string())
    }
}

/// **The platform startup host beside this executable, never through `PATH`.**
///
/// The binaries are built, signed and shipped as one unit, and the first
/// `koru.exe` on somebody's `PATH` could belong to any channel. This is the
/// same rule — and the same shape — the Agent uses to find its siblings; see
/// `spawn_koru` and `session_host::binary`.
///
/// It is checked for existence rather than assumed: a startup entry naming a
/// path that is not there is a sign-in that silently does nothing, and the
/// person finds out months later.
fn agent_executable() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|_| startup_action_unavailable())?;
    let directory = exe.parent().ok_or_else(startup_action_unavailable)?;
    let candidate = agent_executable_beside(directory);
    if !candidate.exists() {
        return Err(startup_action_unavailable());
    }
    Ok(candidate)
}

/// The rule, with the directory passed in — so a test can ask what it names
/// without depending on which binaries happen to have been built.
fn agent_executable_beside(directory: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    return directory.join("koru-agent.exe");
    #[cfg(not(windows))]
    return directory.join("koru");
}

fn startup_command(executable: &std::path::Path) -> String {
    #[cfg(windows)]
    return koru_autostart::quoted(executable);
    #[cfg(not(windows))]
    return format!(
        "{} {}",
        koru_autostart::quoted(executable),
        koru_autostart::KORU_BACKGROUND_ARGUMENT
    );
}

fn startup_action_unavailable() -> String {
    "the startup action is unavailable".to_owned()
}

#[cfg(test)]
mod tests;
