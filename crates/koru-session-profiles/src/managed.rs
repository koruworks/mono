// SPDX-License-Identifier: MPL-2.0

//! Build-authored launch facts. Compatibility is a host handshake.
use koru_session_protocol::{
    ErrorCode, ExecutableAvailability, ProviderKind, ProviderReadiness, SessionMode, capability,
};

/// Ordinary profile-tab preference. Explicit raw access remains available;
/// delegation separately requests managed mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedLaunchPolicy {
    PreferManaged,
    Raw,
}

/// Static configuration only. Arguments are the base managed argv after the
/// executable; per-run settings and credentials stay inside the adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManagedProviderRegistration {
    pub kind: &'static str,
    pub executable: &'static str,
    pub arguments: &'static [&'static str],
    pub launch_policy: ManagedLaunchPolicy,
    pub minimum_host_capability: &'static str,
    /// The command a person runs to update the agent to a version Koru
    /// supports, named on the *Agent providers* page when the installed one
    /// is not; `None` where Koru knows no such command.
    pub update_command: Option<&'static str>,
    /// The command a person runs to sign the agent in, named on the page
    /// when it reports itself signed out; `None` where Koru knows none.
    pub sign_in_command: Option<&'static str>,
    /// The agent's desktop app, by name, when that app installs a copy of
    /// the agent's CLI of its own and keeps it up to date: beside
    /// `update_command` the page then says that copy updates with the app,
    /// so a person who has only the app is not sent to a command that
    /// updates a different copy. `None` for an agent with no such app.
    pub desktop_app: Option<&'static str>,
}

pub const CODEX_REGISTRATION: ManagedProviderRegistration = ManagedProviderRegistration {
    kind: "codex",
    executable: "codex",
    arguments: &["app-server", "--stdio"],
    launch_policy: ManagedLaunchPolicy::PreferManaged,
    minimum_host_capability: capability::MANAGED_SESSIONS,
    update_command: None,
    sign_in_command: None,
    desktop_app: None,
};

pub const CLAUDE_REGISTRATION: ManagedProviderRegistration = ManagedProviderRegistration {
    kind: "claude",
    executable: "claude-agent-acp",
    arguments: &[],
    launch_policy: ManagedLaunchPolicy::Raw,
    minimum_host_capability: capability::MANAGED_SESSIONS,
    update_command: None,
    sign_in_command: None,
    desktop_app: None,
};

/// The npm package that installs `claude-agent-acp`, the program Claude's
/// managed route (a chat) runs.
pub const CLAUDE_ADAPTER_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";

/// **The one `claude-agent-acp` release Claude's managed route accepts** —
/// what its handshake checks (`koru-session-runtime`'s ACP routes) and what
/// *Agent providers*' *Install* installs, so the two cannot name different
/// releases.
pub const CLAUDE_ADAPTER_VERSION: &str = "0.76.0";

pub const OPENCODE_REGISTRATION: ManagedProviderRegistration = ManagedProviderRegistration {
    kind: "opencode",
    executable: "opencode",
    arguments: &["acp"],
    launch_policy: ManagedLaunchPolicy::Raw,
    minimum_host_capability: capability::MANAGED_SESSIONS,
    update_command: None,
    sign_in_command: None,
    desktop_app: None,
};
pub const KIMI_REGISTRATION: ManagedProviderRegistration = ManagedProviderRegistration {
    kind: "kimi",
    executable: "kimi",
    arguments: &["acp"],
    launch_policy: ManagedLaunchPolicy::Raw,
    minimum_host_capability: capability::MANAGED_SESSIONS,
    update_command: None,
    sign_in_command: None,
    desktop_app: None,
};
pub const GROK_REGISTRATION: ManagedProviderRegistration = ManagedProviderRegistration {
    kind: "grok",
    executable: "grok",
    arguments: &["agent", "stdio"],
    launch_policy: ManagedLaunchPolicy::Raw,
    minimum_host_capability: capability::MANAGED_SESSIONS,
    update_command: None,
    sign_in_command: None,
    desktop_app: None,
};

/// Google's official Antigravity ACP server (AGENTUX I9). It is a separate
/// download from the raw `agy` CLI, so, like Claude's wrapper, the managed
/// executable is not the raw profile's program. `scripts/setup/fetch-antigravity-acp.ps1`
/// places it and puts its folder on the user PATH.
pub const ANTIGRAVITY_REGISTRATION: ManagedProviderRegistration = ManagedProviderRegistration {
    kind: "antigravity",
    executable: "agy_acp_server",
    arguments: &[],
    launch_policy: ManagedLaunchPolicy::Raw,
    minimum_host_capability: capability::MANAGED_SESSIONS,
    update_command: None,
    sign_in_command: None,
    desktop_app: None,
};

/// Cognition's Devin CLI as an ACP server over stdio (`devin acp`). The same
/// program the raw tab runs, found on `PATH` or where its installers put it
/// (`resolve_executable`'s known locations). Devin signs itself in; Koru
/// never reads its credentials.
pub const DEVIN_REGISTRATION: ManagedProviderRegistration = ManagedProviderRegistration {
    kind: "devin",
    executable: "devin",
    arguments: &["acp"],
    launch_policy: ManagedLaunchPolicy::Raw,
    minimum_host_capability: capability::MANAGED_SESSIONS,
    // Both as Devin's bundled docs give them.
    update_command: Some("devin update"),
    sign_in_command: Some("devin auth login"),
    // Devin's desktop app carries its own `devin` and updates it itself.
    desktop_app: Some("Devin"),
};

/// Registration grants neither managed readiness nor delegation's live verdict.
pub const MANAGED_PROVIDER_REGISTRATIONS: &[ManagedProviderRegistration] = &[
    CODEX_REGISTRATION,
    CLAUDE_REGISTRATION,
    OPENCODE_REGISTRATION,
    KIMI_REGISTRATION,
    GROK_REGISTRATION,
    ANTIGRAVITY_REGISTRATION,
    DEVIN_REGISTRATION,
];

pub fn managed_provider(kind: &ProviderKind) -> Option<&'static ManagedProviderRegistration> {
    MANAGED_PROVIDER_REGISTRATIONS
        .iter()
        .find(|entry| entry.kind == kind.as_str())
}

/// The caller's intent, separate from a registration's ordinary-tab policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchIntent {
    Ordinary,
    Raw,
    Managed,
    Delegation,
}

/// Decide from authored policy and the host's cached handshake, without I/O.
/// A cached delegation hint only admits a launch attempt; the live seven-point
/// verdict still decides whether that new session may execute a run.
pub fn managed_launch_mode(
    profile: &crate::LaunchProfile,
    intent: LaunchIntent,
    registrations: &[ManagedProviderRegistration],
    snapshot: Option<&[ProviderReadiness]>,
) -> Result<SessionMode, ErrorCode> {
    if intent == LaunchIntent::Raw {
        return Ok(SessionMode::RawPty);
    }
    let registration = profile.provider.as_ref().and_then(|kind| {
        registrations
            .iter()
            .find(|entry| entry.kind == kind.as_str())
    });
    if intent == LaunchIntent::Ordinary
        && registration.is_none_or(|entry| entry.launch_policy == ManagedLaunchPolicy::Raw)
    {
        return Ok(SessionMode::RawPty);
    }
    let provider = profile
        .provider
        .as_ref()
        .ok_or(ErrorCode::ProfileIncompatible)?;
    // Preserve the ordinary managed-preferred tab's existing live attempt:
    // an unfinished cache is not a compatibility refusal or a raw choice.
    if intent == LaunchIntent::Ordinary && snapshot.is_none() {
        return Ok(SessionMode::Managed {
            provider: provider.clone(),
        });
    }
    let row = snapshot
        .and_then(|rows| rows.iter().find(|row| row.kind == *provider))
        .ok_or_else(|| {
            if snapshot.is_none() {
                ErrorCode::ProviderUnavailable
            } else {
                ErrorCode::ProviderIncompatible
            }
        })?;
    match &row.availability {
        ExecutableAvailability::Available { .. } => {}
        ExecutableAvailability::Checking if intent == LaunchIntent::Ordinary => {}
        ExecutableAvailability::Checking => return Err(ErrorCode::ProviderUnavailable),
        ExecutableAvailability::Missing => return Err(ErrorCode::ProfileMissing),
        ExecutableAvailability::SignedOut { code }
        | ExecutableAvailability::Incompatible { code, .. }
        | ExecutableAvailability::Failed { code } => return Err(code.clone()),
    }
    if intent == LaunchIntent::Delegation
        && !row.capabilities.supports(capability::MANAGED_DELEGATION)
    {
        return Err(ErrorCode::CapabilityNotNegotiated);
    }
    Ok(SessionMode::Managed {
        provider: provider.clone(),
    })
}

#[cfg(test)]
mod tests;
