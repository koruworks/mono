// SPDX-License-Identifier: MPL-2.0

use std::sync::OnceLock;

/// This build's own release notes, as Markdown, beside its executables.
///
/// **Written by `koru-release`, read by `koru.exe`, and named in one place so
/// the two cannot disagree.** It lives here rather than in `koru-update`
/// because the reader dropped that dependency deliberately — reaching a
/// filename through it would put reqwest, zip and minisign-verify back in the
/// graph of a window that only wants to render a page.
///
/// **Absence is ordinary.** A `cargo` build has never been released and a
/// `dist-local` bundle has no descriptor to render, so every reader treats a
/// missing file as "no notes for this build" rather than as an error.
pub const RELEASE_NOTES_FILE: &str = "RELEASE_NOTES.md";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: &'static str,
    pub build_number: u64,
    pub channel: &'static str,
    pub commit: &'static str,
    /// The instant this build was published, RFC 3339 in UTC — the same one the
    /// channel manifest records as `published_at` — stamped by `koru-release`.
    /// Empty for a local cargo build, which was never released at all; see
    /// [`BuildInfo::released`].
    pub released_at: &'static str,
    pub platform: &'static str,
    pub architecture: &'static str,
    pub updater_protocol_version: u32,
    pub update_public_key: &'static str,
    pub updater_sha256: &'static str,
}

impl BuildInfo {
    pub fn updates_enabled(self) -> bool {
        self.build_number > 0
            && !self.update_public_key.trim().is_empty()
            && !self.updater_sha256.trim().is_empty()
    }

    /// When this build was released, when it was released at all.
    ///
    /// A local cargo build is not a release, so this is `None` there rather
    /// than a compile time dressed up as one — Welcome falls back to the
    /// executable's timestamp and says `built` instead of `released`.
    pub fn released(self) -> Option<&'static str> {
        Some(self.released_at.trim()).filter(|stamp| !stamp.is_empty())
    }

    /// What the Welcome "Current:" line shows after the build number — a real
    /// commit when the release tool stamped one, otherwise `debug` / `release`
    /// for local cargo builds (never the opaque "unknown").
    pub fn build_label(self) -> &'static str {
        if self.commit != "unknown" && !self.commit.is_empty() {
            self.commit
        } else if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    }
}

pub fn build_info() -> &'static BuildInfo {
    static INFO: OnceLock<BuildInfo> = OnceLock::new();
    INFO.get_or_init(|| BuildInfo {
        version: option_env!("KORU_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),
        build_number: option_env!("KORU_BUILD_NUMBER")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        // Local cargo builds are internal — not a published channel.
        channel: option_env!("KORU_RELEASE_CHANNEL").unwrap_or("internal"),
        commit: option_env!("KORU_COMMIT").unwrap_or("unknown"),
        released_at: option_env!("KORU_RELEASED_AT").unwrap_or(""),
        platform: platform(),
        architecture: architecture(),
        updater_protocol_version: 1,
        update_public_key: option_env!("KORU_UPDATE_PUBLIC_KEY").unwrap_or(""),
        updater_sha256: option_env!("KORU_UPDATER_SHA256").unwrap_or(""),
    })
}

const fn platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "android") {
        // A phone's `target_os` is `android`, never `linux`, so it was
        // `unknown` here; it is the update feed's platform value (A6).
        "android"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

const fn architecture() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86") {
        "x86"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamped(released_at: &'static str) -> BuildInfo {
        BuildInfo {
            released_at,
            ..*build_info()
        }
    }

    /// Welcome takes the `None` as "this was never released" and shows a built
    /// time instead, so an unstamped build must not answer with a blank string
    /// — that would print `build 0 · released  ·`.
    #[test]
    fn only_a_published_build_has_a_release_time() {
        assert_eq!(stamped("").released(), None);
        assert_eq!(stamped("   ").released(), None);
        assert_eq!(
            stamped("2026-08-08T14:32:11Z").released(),
            Some("2026-08-08T14:32:11Z")
        );
    }
}
