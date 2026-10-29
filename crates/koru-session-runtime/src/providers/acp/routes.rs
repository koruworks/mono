// SPDX-License-Identifier: MPL-2.0

//! Exact CLI dialects and immutable process policy; no registration side effects.
use super::*;
use koru_session_profiles::{self as profiles, ManagedProviderRegistration};
use koru_session_protocol::ProviderKind;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AcpDialect {
    Generic,
    Claude,
    OpenCode,
    Kimi,
    Grok,
    Antigravity,
}

impl AcpDialect {
    pub(super) fn registration(self) -> Option<ManagedProviderRegistration> {
        Some(match self {
            Self::Generic => return None,
            Self::Claude => profiles::CLAUDE_REGISTRATION,
            Self::OpenCode => profiles::OPENCODE_REGISTRATION,
            Self::Kimi => profiles::KIMI_REGISTRATION,
            Self::Grok => profiles::GROK_REGISTRATION,
            Self::Antigravity => profiles::ANTIGRAVITY_REGISTRATION,
        })
    }

    /// Google's `agy_acp_server.exe` has no `--version` flag (absl rejects
    /// it), so the executable is identified by its SHA-256 instead: the
    /// "printed" half of each pin is that digest. Its `initialize` names the
    /// version too, and the handshake checks the two agree.
    pub(super) fn identified_by_digest(self) -> bool {
        self == Self::Antigravity
    }

    /// Whether closing stdin ends the process. Antigravity's server was
    /// measured still running 60 s after EOF, with or without a session, so
    /// its shutdown is the owned tree's termination, not an EOF wait.
    pub(super) fn exits_on_eof(self) -> bool {
        self != Self::Antigravity
    }

    /// Whether each launch unpacks itself into the temp folder. Antigravity's
    /// one-file bundle writes a 312 MB `_MEI*` folder there per launch and
    /// deletes it only on an orderly exit — which, never exiting on EOF, it
    /// does not get — so each launch gets a private TMP the owner removes.
    pub(super) fn unpacks_into_temp(self) -> bool {
        self == Self::Antigravity
    }

    /// The shortest request budget the route can start under. Antigravity's
    /// server is a one-file bundle that unpacks itself on every launch: its
    /// `initialize` answered in 7.0-8.5 s on the acceptance machine, over the
    /// 5 s readiness budget every other route fits in.
    pub(super) fn request_timeout_floor(self) -> Duration {
        match self {
            Self::Antigravity => Duration::from_secs(20),
            _ => Duration::ZERO,
        }
    }
    /// The version number in a `--version` print that is not one of the
    /// pinned ones: the whole print for the CLIs that print a bare number,
    /// the word after `grok` for Grok, nothing for a digest-identified
    /// route. The caller checks it against the pin.
    pub(super) fn printed_version(self, printed: &str) -> Option<&str> {
        fn bare(text: &str) -> Option<&str> {
            let text = text.strip_prefix('v').unwrap_or(text);
            (text.starts_with(|c: char| c.is_ascii_digit())
                && text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')))
            .then_some(text)
        }
        match self {
            Self::Generic | Self::Antigravity => None,
            Self::Claude | Self::OpenCode | Self::Kimi => bare(printed),
            // `grok <version> (<build>) [<channel>]`, that shape and no
            // other: an adorned print is still refused.
            Self::Grok => {
                let words: Vec<&str> = printed.split_ascii_whitespace().collect();
                match words.as_slice() {
                    ["grok", version, build, channel]
                        if build.starts_with('(')
                            && build.ends_with(')')
                            && channel.starts_with('[')
                            && channel.ends_with(']') =>
                    {
                        bare(version)
                    }
                    _ => None,
                }
            }
        }
    }

    /// Whether a release newer than the pin is admitted: the CLIs that
    /// print a version number are; a digest-identified route and a route
    /// built by hand with its own list (the Generic dialect) are not.
    pub(super) fn admits_newer_releases(self) -> bool {
        matches!(
            self,
            Self::Claude | Self::OpenCode | Self::Kimi | Self::Grok
        )
    }

    /// The qualified pin: the first accepted version, which the probe logs
    /// as the expected one.
    pub(super) fn version(self) -> &'static str {
        self.versions().first().map_or("", |(_, version)| version)
    }

    /// Every accepted version, as the exact `--version` output the CLI
    /// prints and the version it names. The first is the qualified pin;
    /// the later ones were admitted on the maintainers' instruction (AGENTUX,
    /// 2026-09-25) after the no-prompt factory handshake passed on the
    /// installed CLI. Each provider note says what that does and does not
    /// prove.
    pub(super) fn versions(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Generic => &[],
            Self::Claude => &[("0.76.0", "0.76.0")],
            Self::OpenCode => &[("1.18.27", "1.18.27")],
            // 2.3.3 and 2.4.0: the normally installed CLI on 2026-10-27 and 2026-10-28 (UPDATEAGENTS),
            // admitted on the same factory handshake as 2.0.0.
            Self::Kimi => &[
                ("0.42.0", "0.42.0"),
                ("2.0.0", "2.0.0"),
                ("2.3.3", "2.3.3"),
                ("2.4.0", "2.4.0"),
            ],
            Self::Grok => &[
                ("grok 1.0.25 (f7e67d6988e2) [stable]", "1.0.25"),
                ("grok 1.0.34 (3736acbc8658) [stable]", "1.0.34"),
                // The normally installed CLI on 2026-10-27 (UPDATEAGENTS),
                // admitted on the same no-prompt factory handshake as 1.0.34;
                // `docs/features/terminal/providers/grok.md` has the line.
                ("grok 1.0.60 (01a8e1c591c0) [stable]", "1.0.60"),
                ("grok 1.0.70 (f50904eaee6c) [stable]", "1.0.70"),
            ],
            // SHA-256 of `agy_acp_server.exe` from Google's
            // agy-acp-server-1.2.1-windows-x86_64.zip (AGENTUX I9, 2026-09-26).
            Self::Antigravity => &[(
                "04fd51b5c6dfb6d11e649c2cfc61a64ca31d9a4a58a9a3e5c93db934cd7a9695",
                "1.2.1",
            )],
        }
    }
}

impl AcpRoute {
    fn named(dialect: AcpDialect) -> Self {
        let registration = dialect.registration().expect("named route");
        let mut route = Self::new(
            ProviderKind::from_str(registration.kind).expect("built-in provider kind"),
            dialect
                .versions()
                .iter()
                .map(|(_, version)| ProviderVersion::from_str(version).expect("pinned version"))
                .collect(),
            registration.arguments.iter().map(OsString::from).collect(),
            ChildSessionPolicy::Uncontrolled,
        );
        route.dialect = dialect;
        route.enforce_environment();
        route
    }

    /// Exact qualified wrapper; its raw profile remains independent.
    pub fn claude_code() -> Self {
        let mut route = Self::named(AcpDialect::Claude);
        route.child_sessions = ChildSessionPolicy::Disabled;
        route.session_new_meta = Some(json!({
            "claudeCode":{"options":{"disallowedTools":["Agent","Task"]}}
        }));
        route
    }
    pub fn opencode() -> Self {
        let mut route = Self::named(AcpDialect::OpenCode);
        route.child_sessions = ChildSessionPolicy::Experimental;
        route
    }
    pub fn kimi() -> Self {
        let mut route = Self::named(AcpDialect::Kimi);
        route.child_sessions = ChildSessionPolicy::Experimental;
        route
    }
    pub fn grok() -> Self {
        let mut route = Self::named(AcpDialect::Grok);
        route.child_sessions = ChildSessionPolicy::Disabled;
        route.cached_auth_method = Some("cached_token".to_owned());
        route.authenticate_meta = Some(json!({"headless":true}));
        route
    }
    /// Google's official ACP server for Antigravity. No `authenticate` call:
    /// its OAuth methods open a browser when no credential is cached, so the
    /// server's own `settings.json` choice is what a session starts under,
    /// and a missing sign-in is `session/new`'s -32000. Child sessions stay
    /// uncontrolled — nothing proves how it spawns sub-agents — which keeps
    /// the route out of delegation; a chat does not delegate.
    pub fn antigravity() -> Self {
        Self::named(AcpDialect::Antigravity)
    }

    pub(super) fn is_antigravity(&self) -> bool {
        self.dialect == AcpDialect::Antigravity
    }

    pub(super) fn is_grok(&self) -> bool {
        self.dialect == AcpDialect::Grok
    }

    pub(super) fn is_claude(&self) -> bool {
        self.dialect == AcpDialect::Claude
    }

    pub(super) fn enforce_environment(&mut self) {
        let mandatory: &[(&str, &str)] = match self.dialect {
            AcpDialect::Grok => &[("GROK_SUBAGENTS", "0"), ("GROK_DISABLE_AUTOUPDATER", "1")],
            _ => &[],
        };
        for (key, value) in mandatory {
            self.environment
                .retain(|(name, _)| !name.to_string_lossy().eq_ignore_ascii_case(key));
            self.environment.push((key.into(), value.into()));
        }
    }

    /// Only this closed route table may append tokens after a Windows batch
    /// launcher. Author-authored arguments still never cross a second parser.
    pub(super) fn launch(
        &self,
        executable: &OsStr,
        version: bool,
    ) -> Result<(OsString, Vec<OsString>), ProviderFailure> {
        let Some(registration) = self.dialect.registration() else {
            return Ok((
                executable.to_owned(),
                if version {
                    vec!["--version".into()]
                } else {
                    self.arguments.clone()
                },
            ));
        };
        let mut launch = profiles::platform_launch_argv(PathBuf::from(executable), &[])
            .ok_or_else(invalid_launch)?;
        let program = launch.remove(0);
        if !launch.is_empty() {
            let fixed: Vec<OsString> = registration.arguments.iter().map(OsString::from).collect();
            if self.arguments != fixed {
                return Err(invalid_launch());
            }
        }
        if version {
            launch.push("--version".into());
        } else {
            launch.extend(self.arguments.clone());
        }
        Ok((program, launch))
    }

    pub(super) fn parse_version(&self, output: &str) -> Result<ProviderVersion, ProviderFailure> {
        let rejected = || {
            failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::VersionRejected,
            )
        };
        if self.dialect == AcpDialect::Generic {
            let token = output
                .split_ascii_whitespace()
                .last()
                .ok_or_else(rejected)?;
            return ProviderVersion::from_str(token.strip_prefix('v').unwrap_or(token));
        }
        let printed = output.trim();
        if let Some((_, version)) = self
            .dialect
            .versions()
            .iter()
            .find(|(expected, _)| *expected == printed)
        {
            return ProviderVersion::from_str(version);
        }
        // Not a pinned print: read the number the CLI printed and admit it
        // when it is at or above the qualified pin (the CTO's note). A
        // digest-identified route has no number to read.
        let token = self.dialect.printed_version(printed).ok_or_else(rejected)?;
        let version = ProviderVersion::from_str(token)?;
        if self.supports(&version) {
            Ok(version)
        } else {
            Err(rejected())
        }
    }
}

fn invalid_launch() -> ProviderFailure {
    failure(
        ErrorCode::InvalidRequest,
        ProviderStage::Spawn,
        ProviderDiagnostic::InvalidInput,
    )
}
