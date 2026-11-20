// SPDX-License-Identifier: MPL-2.0

use koru_session_protocol::{SessionMode, SessionOrigin};
use koru_session_wire::{CreateProfileSession, LaunchBootstrap};

/// Called only after replay/admission on the host's bounded blocking lane.
pub(super) fn capture(
    request: &CreateProfileSession,
    bootstrap: &LaunchBootstrap,
) -> Option<koru_delegate_preamble::Resolved> {
    eligible(&request.mode, request.origin, request.delegation.is_some()).then(|| {
        let configuration = std::env::current_exe()
            .ok()
            .and_then(|exe| {
                let directory = exe.parent()?;
                let installation = if cfg!(target_os = "macos")
                    && directory.file_name().is_some_and(|name| name == "MacOS")
                {
                    directory.parent()?.join("Resources")
                } else {
                    directory.to_path_buf()
                };
                let settings = koru_settings::SettingsPaths::for_channel_root(
                    &bootstrap.root,
                    koru_channel::channel_root().as_deref(),
                    &bootstrap.segment,
                );
                Some(koru_delegate_preamble::session::Configuration::files(
                    installation,
                    settings.map(|paths| paths.session_instructions_file()),
                ))
            })
            .unwrap_or_default();
        configuration.resolve()
    })
}

fn eligible(mode: &SessionMode, origin: SessionOrigin, delegated: bool) -> bool {
    matches!(mode, SessionMode::Managed { .. }) && origin == SessionOrigin::Manual && !delegated
}

#[cfg(test)]
mod tests;
