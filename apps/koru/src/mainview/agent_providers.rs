// SPDX-License-Identifier: MPL-2.0

//! **Settings → Coding → Agent providers**, the window's half.
//!
//! The settings page draws the rows; this module is everything behind them:
//! which agents there are (`koru_session_profiles::coding_agents`, the same
//! list the terminal's *Agents ›* menu shows), how ready each is, where its
//! setup guide is, and the switch that hides it.
//!
//! **The status is read, not probed again here.** Whether an agent's
//! program is on `PATH` is the `+` menu's presence probe
//! ([`ProfileProbe`](super::profile_probe::ProfileProbe), off the loop
//! thread); whether an installed agent is signed out, on a version Koru does
//! not accept, or missing the program its chats run through is the session
//! host's readiness snapshot, which the window already holds. *Refresh
//! status* re-runs those two checks and nothing else.
//!
//! **The switch is a device-local setting** (`GlobalSettings::disabled_agents`
//! in `desktop-v1.json`), written through the window's Global link. Off hides
//! the agent from the chat's agent rail, from *Agents ›* and from every new
//! session choice; a chat or a session already on it keeps working, and
//! nothing is uninstalled or signed out.

use koru_session_profiles::{LaunchProfile, managed_provider};
use koru_session_protocol::{ExecutableAvailability, ProviderReadiness};
use koru_settings_ui::{AgentAttention, AgentProviderRow, AgentProviderStatus};

use super::{MainView, TabView, context_menu};

/// **Each agent's install or setup page** — the vendor's own documentation,
/// opened by the row's *Setup guide ↗*. Keyed by agent key; an agent with no
/// entry shows no link. No per-agent page existed in the code before this;
/// each was checked to answer on 2026-11-02.
pub(crate) fn setup_guide(key: &str) -> Option<&'static str> {
    Some(match key {
        "claude" => "https://code.claude.com/docs/en/setup",
        "codex" => "https://developers.openai.com/codex/cli",
        "opencode" => "https://opencode.ai/docs/",
        "grok" => "https://docs.x.ai/build/overview#install",
        "kimi" => "https://moonshotai.github.io/kimi-code/en/",
        "copilot" => {
            "https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/install-copilot-cli"
        }
        "qwen" => "https://github.com/QwenLM/qwen-code",
        "antigravity" => "https://antigravity.google/docs",
        "muse" => "https://dev.meta.ai/products/muse-code",
        _ => return None,
    })
}

/// **One agent's status, from what Koru already knows.**
///
/// `present` is the presence probe's answer for the agent's own program
/// (`None` until it has answered); `readiness` is the session host's
/// snapshot (`None` until the first one). An agent with a managed route is
/// what its readiness row says once its program is there; one without (the
/// terminal-only agents) is ready when its program is.
pub(crate) fn agent_status(
    profile: &LaunchProfile,
    present: Option<bool>,
    readiness: Option<&[ProviderReadiness]>,
) -> AgentProviderStatus {
    let program = profile.argv.first().cloned().unwrap_or_default();
    match present {
        None => return AgentProviderStatus::Checking,
        Some(false) => return AgentProviderStatus::NotInstalled { program },
        Some(true) => {}
    }
    let Some(key) = profile.label.agent_key() else {
        return AgentProviderStatus::Ready;
    };
    let Some(registration) = key.parse().ok().and_then(|kind| managed_provider(&kind)) else {
        return AgentProviderStatus::Ready;
    };
    let Some(readiness) = readiness else {
        return AgentProviderStatus::Checking;
    };
    let Some(row) = readiness
        .iter()
        .find(|row| row.kind.as_str() == registration.kind)
    else {
        return AgentProviderStatus::Ready;
    };
    match &row.availability {
        ExecutableAvailability::Available { .. } => AgentProviderStatus::Ready,
        ExecutableAvailability::Checking => AgentProviderStatus::Checking,
        // The agent's own program is here, so what is missing is the one
        // its chats run through — Claude's wrapper, Antigravity's server.
        ExecutableAvailability::Missing => {
            AgentProviderStatus::NeedsAttention(AgentAttention::AdapterMissing {
                program: registration.executable.to_owned(),
            })
        }
        ExecutableAvailability::SignedOut { .. } => {
            AgentProviderStatus::NeedsAttention(AgentAttention::SignedOut)
        }
        ExecutableAvailability::Incompatible { .. } => {
            AgentProviderStatus::NeedsAttention(AgentAttention::UnsupportedVersion)
        }
        ExecutableAvailability::Failed { .. } => {
            AgentProviderStatus::NeedsAttention(AgentAttention::CheckFailed)
        }
    }
}

impl MainView {
    /// The agents turned off on this machine, as the Global link last read
    /// (or was just told) them.
    pub(super) fn disabled_agents(&self) -> &[String] {
        &self.delegate_folder.authoritative().disabled_agents
    }

    /// Every coding agent as *Agent providers* shows it.
    pub(super) fn agent_provider_rows(&self) -> Vec<AgentProviderRow> {
        let disabled = self.disabled_agents();
        let readiness = self.provider_readiness.as_deref();
        koru_session_profiles::coding_agents()
            .into_iter()
            .filter_map(|(index, profile)| {
                let key = profile.label.agent_key()?;
                Some(AgentProviderRow {
                    key: key.to_owned(),
                    name: koru_session_profiles::display_name(&profile, "", "").to_owned(),
                    status: agent_status(&profile, self.profile_probe.presence(index), readiness),
                    guide: setup_guide(key),
                    on: !disabled.iter().any(|off| off == key),
                })
            })
            .collect()
    }

    /// Hand the rows to every Settings tab. Answers whether one moved.
    pub(super) fn sync_agent_providers(&mut self) -> bool {
        let rows = self.agent_provider_rows();
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Settings(view) => view.set_agent_providers(rows.clone()),
            _ => false,
        })
    }

    /// Hand the off list to every chat surface — the side pane's and every
    /// chat tab's — and to the menus, and to chat tabs opened later.
    pub(super) fn apply_hidden_agents(&mut self) -> bool {
        let keys = self.disabled_agents().to_vec();
        crate::ai_chat_view::set_hidden_agents(keys.clone());
        context_menu::set_hidden_agents(keys.clone());
        let mut changed = self.ai_chat.set_hidden_agents(keys.clone());
        changed |= self.each_tab_mut(|tab| match &mut tab.view {
            TabView::AiChat(view) => view.set_hidden_agents(keys.clone()),
            _ => false,
        });
        changed
    }

    /// **Turn one agent on or off**: store the whole list on this machine,
    /// and hide or show the agent everywhere at once.
    pub(super) fn set_agent_enabled(&mut self, key: &str, on: bool) -> bool {
        let mut list: Vec<String> = self
            .disabled_agents()
            .iter()
            .filter(|off| *off != key)
            .cloned()
            .collect();
        if !on {
            list.push(key.to_owned());
        }
        self.save_disabled_agents(list)
    }

    /// *Restore defaults*: every agent on.
    pub(super) fn restore_agent_defaults(&mut self) -> bool {
        self.save_disabled_agents(Vec::new())
    }

    fn save_disabled_agents(&mut self, list: Vec<String>) -> bool {
        self.delegate_folder.save(
            koru_settings::GlobalPatch {
                disabled_agents: Some(list),
                ..koru_settings::GlobalPatch::default()
            },
            |patch, section| patch.clone().apply(section),
        );
        self.apply_hidden_agents();
        self.sync_agent_providers();
        true
    }

    /// *Refresh status*: re-run the existing checks, both off the loop
    /// thread — the presence probe on its worker, and the session host's
    /// readiness probe for every managed agent. Their answers land through
    /// the loop's wake and redraw the rows.
    pub(super) fn refresh_agent_status(&mut self) -> bool {
        self.profile_probe.refresh(self.agent.wake_proxy());
        for registration in koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS {
            if let Ok(kind) = registration.kind.parse() {
                self.refresh_managed_provider(kind);
            }
        }
        self.sync_agent_providers()
    }

    /// What opening the page needs: a fresh presence answer, and the session
    /// host's readiness, both asked for off the loop thread.
    pub(super) fn prepare_agent_providers(&mut self) {
        self.profile_probe.refresh(self.agent.wake_proxy());
        self.ensure_provider_readiness();
        self.sync_agent_providers();
    }
}

#[cfg(test)]
mod tests;
