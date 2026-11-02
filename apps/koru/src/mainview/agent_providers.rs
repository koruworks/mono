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
//!
//! **Where a new chat starts** is the same kind of setting, beside it: the
//! agent it starts with (`default_agent`, D40) and the permission a new chat
//! with a supported agent starts in and the one any other agent's does
//! (`default_permission`, `default_permission_other`, D39). Every chat
//! surface is handed them; only a new chat reads them. A default agent that
//! is turned off or not installed falls back to *Auto* — the last agent
//! used — in the chat, and the page says so.
//!
//! **The order the agents are listed in** (`agent_order`, D41) is dragged on
//! the page and followed everywhere an agent is chosen: the chat's agent
//! rail, the terminal's *Agents ›* menu, the *Default agent* chips (the
//! page's own rows) and the delegate dialog. [`agent_order`] is the one
//! place the stored order meets the built-in one.

use koru_session_profiles::{LaunchProfile, managed_provider};
use koru_session_protocol::{ExecutableAvailability, ProviderReadiness};
use koru_settings::AgentPermission;
use koru_settings_ui::{AgentAttention, AgentDefaults, AgentProviderRow, AgentProviderStatus};

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

/// **The order the coding agents are listed in**: the keys `stored` names,
/// in its order, then every other coding agent in the built-in order —
/// Claude first, then the *Agents ›* menu's order — so an empty `stored` is
/// the built-in order and an agent a later Koru adds is listed, last. A key
/// no agent has is left out.
pub(crate) fn agent_order(stored: &[String]) -> Vec<String> {
    let built_in: Vec<&'static str> = koru_session_profiles::coding_agents()
        .into_iter()
        .filter_map(|(_, profile)| profile.label.agent_key())
        .collect();
    let mut order: Vec<String> = Vec::with_capacity(built_in.len());
    for key in stored {
        if built_in.contains(&key.as_str()) && !order.contains(key) {
            order.push(key.clone());
        }
    }
    for key in built_in {
        if !order.iter().any(|known| known == key) {
            order.push(key.to_owned());
        }
    }
    order
}

/// Where `key` sorts in `order`: its place, and after every listed agent
/// for one it does not name (a shell, a custom profile).
pub(crate) fn order_rank(order: &[String], key: Option<&str>) -> usize {
    key.and_then(|key| order.iter().position(|known| known == key))
        .unwrap_or(usize::MAX)
}

/// **Why the default agent is not what a new chat gets**, in the page's
/// words: it is turned off, or its program is not on this computer. `None`
/// for *Auto*, an agent that can be had, and one the page does not list.
pub(crate) fn default_agent_note(agent: Option<&str>, rows: &[AgentProviderRow]) -> Option<String> {
    let row = rows.iter().find(|row| Some(row.key.as_str()) == agent)?;
    let lang = crate::format::lang();
    let words = if !row.on {
        koru_i18n::Key::SettingsDefaultAgentOff
    } else if matches!(row.status, AgentProviderStatus::NotInstalled { .. }) {
        koru_i18n::Key::SettingsDefaultAgentMissing
    } else {
        return None;
    };
    Some(koru_i18n::fill(koru_i18n::t(lang, words), &[&row.name]))
}

/// A stored permission in the chat surface's words.
pub(crate) fn new_chat_permission(permission: AgentPermission) -> koru_ai_ui::NewChatPermission {
    match permission {
        AgentPermission::Manual => koru_ai_ui::NewChatPermission::Manual,
        AgentPermission::Auto => koru_ai_ui::NewChatPermission::Auto,
        AgentPermission::Yolo => koru_ai_ui::NewChatPermission::Yolo,
    }
}

impl MainView {
    /// The agents turned off on this machine, as the Global link last read
    /// (or was just told) them.
    pub(super) fn disabled_agents(&self) -> &[String] {
        &self.delegate_folder.authoritative().disabled_agents
    }

    /// Where a new chat starts, as *Agent providers* shows it beside
    /// `rows`: with a note when the default agent is turned off or not
    /// installed, and so falls back to *Auto*.
    pub(super) fn agent_defaults(&self, rows: &[AgentProviderRow]) -> AgentDefaults {
        let section = self.delegate_folder.authoritative();
        AgentDefaults {
            permission: section.default_permission,
            permission_other: section.default_permission_other,
            agent: section.default_agent.clone(),
            agent_note: default_agent_note(section.default_agent.as_deref(), rows),
        }
    }

    /// Where a new chat starts, in the chat surface's words. The surface
    /// itself falls back to *Auto* for an agent it hides or knows is not
    /// installed.
    pub(super) fn chat_agent_defaults(&self) -> koru_ai_ui::AgentDefaults {
        let section = self.delegate_folder.authoritative();
        koru_ai_ui::AgentDefaults {
            permission: new_chat_permission(section.default_permission),
            permission_other: new_chat_permission(section.default_permission_other),
            agent: section.default_agent.clone(),
        }
    }

    /// Everything a chat surface is handed from *Agent providers*: the
    /// agents turned off, and where a new chat starts. What
    /// `maintain_delegate_folder` compares to tell whether to hand it on.
    pub(super) fn agent_settings_snapshot(
        &self,
    ) -> (Vec<String>, koru_ai_ui::AgentDefaults, Vec<String>) {
        (
            self.disabled_agents().to_vec(),
            self.chat_agent_defaults(),
            self.agent_order(),
        )
    }

    /// The order the agents are listed in, every agent named.
    pub(super) fn agent_order(&self) -> Vec<String> {
        agent_order(&self.delegate_folder.authoritative().agent_order)
    }

    /// Every coding agent as *Agent providers* shows it, in the person's
    /// order.
    pub(super) fn agent_provider_rows(&self) -> Vec<AgentProviderRow> {
        let disabled = self.disabled_agents();
        let readiness = self.provider_readiness.as_deref();
        let order = self.agent_order();
        let mut rows: Vec<AgentProviderRow> = koru_session_profiles::coding_agents()
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
            .collect();
        rows.sort_by_key(|row| order_rank(&order, Some(&row.key)));
        rows
    }

    /// Hand the rows, and where a new chat starts, to every Settings tab.
    /// Answers whether one moved.
    pub(super) fn sync_agent_providers(&mut self) -> bool {
        let rows = self.agent_provider_rows();
        let defaults = self.agent_defaults(&rows);
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Settings(view) => {
                let rows_moved = view.set_agent_providers(rows.clone());
                view.set_agent_defaults(defaults.clone()) || rows_moved
            }
            _ => false,
        })
    }

    /// Hand the off list, and where a new chat starts, to every chat surface
    /// — the side pane's and every chat tab's — and the off list to the
    /// menus, and both to chat tabs opened later.
    pub(super) fn apply_hidden_agents(&mut self) -> bool {
        let keys = self.disabled_agents().to_vec();
        let defaults = self.chat_agent_defaults();
        let order = self.agent_order();
        crate::ai_chat_view::set_hidden_agents(keys.clone());
        crate::ai_chat_view::set_agent_defaults(defaults.clone());
        crate::ai_chat_view::set_agent_order(order.clone());
        context_menu::set_hidden_agents(keys.clone());
        context_menu::set_agent_order(order.clone());
        let mut changed = self.ai_chat.set_hidden_agents(keys.clone());
        changed |= self.ai_chat.set_agent_defaults(defaults.clone());
        changed |= self.ai_chat.set_agent_order(order.clone());
        changed |= self.each_tab_mut(|tab| match &mut tab.view {
            TabView::AiChat(view) => {
                let hidden = view.set_hidden_agents(keys.clone());
                let defaults = view.set_agent_defaults(defaults.clone());
                view.set_agent_order(order.clone()) || hidden || defaults
            }
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

    /// *Restore defaults*: every agent on, and new chats back to *Auto*
    /// and *Manual*.
    pub(super) fn restore_agent_defaults(&mut self) -> bool {
        self.save_agent_settings(koru_settings::GlobalPatch {
            disabled_agents: Some(Vec::new()),
            default_permission: Some(AgentPermission::Manual),
            default_permission_other: Some(AgentPermission::Manual),
            default_agent: koru_settings::Field::Clear,
            agent_order: Some(Vec::new()),
            ..koru_settings::GlobalPatch::default()
        })
    }

    /// **The order the agents are listed in**, as the page was dragged:
    /// stored on this machine — nothing when it is the built-in order — and
    /// handed to every list of agents at once.
    pub(super) fn set_agent_order(&mut self, keys: Vec<String>) -> bool {
        let order = agent_order(&keys);
        let stored = if order == agent_order(&[]) {
            Vec::new()
        } else {
            order
        };
        self.save_agent_settings(koru_settings::GlobalPatch {
            agent_order: Some(stored),
            ..koru_settings::GlobalPatch::default()
        })
    }

    /// **The agent new chats start with**: a key, or `None` for *Auto*.
    /// Stored on this machine and handed to every chat surface.
    pub(super) fn set_default_agent(&mut self, agent: Option<String>) -> bool {
        self.save_agent_settings(koru_settings::GlobalPatch {
            default_agent: match agent {
                Some(key) => koru_settings::Field::Set(key),
                None => koru_settings::Field::Clear,
            },
            ..koru_settings::GlobalPatch::default()
        })
    }

    /// **The permission new chats start in**, for the supported agents or
    /// (`other`) the rest: stored on this machine, and handed to every chat
    /// surface for its next new chat.
    pub(super) fn set_agent_permission(
        &mut self,
        other: bool,
        permission: AgentPermission,
    ) -> bool {
        // An agent with no auto mode has no *Auto*; the page offers none.
        if other && permission == AgentPermission::Auto {
            return false;
        }
        let patch = if other {
            koru_settings::GlobalPatch {
                default_permission_other: Some(permission),
                ..koru_settings::GlobalPatch::default()
            }
        } else {
            koru_settings::GlobalPatch {
                default_permission: Some(permission),
                ..koru_settings::GlobalPatch::default()
            }
        };
        self.save_agent_settings(patch)
    }

    fn save_disabled_agents(&mut self, list: Vec<String>) -> bool {
        self.save_agent_settings(koru_settings::GlobalPatch {
            disabled_agents: Some(list),
            ..koru_settings::GlobalPatch::default()
        })
    }

    /// Store one *Agent providers* change through the window's Global link
    /// (its worker writes the file), and hand the result to everything that
    /// shows it.
    fn save_agent_settings(&mut self, patch: koru_settings::GlobalPatch) -> bool {
        self.delegate_folder
            .save(patch, |patch, section| patch.clone().apply(section));
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
