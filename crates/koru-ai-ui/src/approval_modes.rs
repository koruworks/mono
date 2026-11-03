// SPDX-License-Identifier: MPL-2.0

//! **The approval-mode picker** of a local agent's chat (koru-ai D7).
//!
//! A ghost chip in the composer's toolbar, left of the model chip, names the
//! mode the chat's next message runs in — its icon, its name and a caret —
//! and opens a list upward, like the composer's other panels: each of the
//! agent's modes with an icon, a title, a line saying what it does, and a
//! check on the current one. *Full access* is in the warning color.
//!
//! **The agent's own modes, in Koru's words.** An agent lists its modes
//! with its catalog (`AgentModelsResponse::modes`); the ones Koru knows —
//! Claude Code's, Codex's three, OpenCode's two — are worded here, and any
//! other shows the agent's own name and description with a neutral icon. An
//! agent that lists none has no chip.
//!
//! **Remembered per chat.** The choice is the chat's (`Chat::agent_mode`). A
//! new chat starts from the person's **default permission** (Settings →
//! Coding → Agent providers, D39): *Manual*, *Auto* or *Yolo* for an agent
//! with an auto mode, *Manual* or *Yolo* for any other, mapped onto the
//! agent's own modes by [`default_mode`] — the one place that mapping is
//! made. A change inside a chat stays that chat's. *Yolo* is the only way a
//! new chat starts in *Full access*: the person chose it there.
//!
//! **Shift+Tab** steps to the next mode, which the chip then names. It
//! skips *Full access*. The composer is asked first, so an outdent would
//! win where there is something to outdent (`koru_keys`' `CycleAgentMode`).
//! No note repeats the mode's name beside the chip (D65, the user: *the name
//! is clear*): the chip's label already reads it.
//!
//! A mode chosen while a reply runs applies to the next message, and the
//! note beside the chip says so — the one thing the chip does not.

use std::time::{Duration, Instant};

use koru_ai_core::AiProviderId;
use koru_ai_wire::{AgentModeView, UpdateChatAgentSettingsRequest};
use koru_draw::geometry::Rect;
use koru_draw::{icons, scene, theme, theme::role};
use koru_i18n::{Key as Words, fill, t};
use koru_ui::button::ActionButton;
use koru_ui::tooltip::TooltipContent;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::agents::is_agent_provider;
use crate::reader::Command;

/// One row of the list: an icon, a title and a line under it.
pub(crate) const MODE_ROW_H: f32 = 46.0;
/// The air around the rows inside the panel.
const PANEL_PAD: f32 = 4.0;
/// How long the note beside the chip stays.
const NOTE_HOLD: Duration = Duration::from_millis(2200);
/// The note's tooltip key, apart from every hover tooltip's.
const NOTE_KEY: u64 = 0xA9_0D_E5;

/// An agent's modes as its listing carried them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ListedModes {
    pub modes: Vec<AgentModeView>,
    /// The mode the agent reports it starts in: a chat that chose none.
    pub default: Option<String>,
}

/// One mode as the picker shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModeRow {
    pub id: String,
    pub title: String,
    pub help: Option<String>,
    pub icon: char,
    /// *Full access*: drawn in the warning color, never stepped to by
    /// Shift+Tab, and a new chat's only by a *Yolo* default.
    pub full_access: bool,
}

/// The modes Koru knows: the agent's kind, the mode's id, its title, its
/// line, its icon, and its [`Role`]. A title or line that is `None` stays
/// the agent's own — a mode can need Koru's role without Koru's words.
const KNOWN: &[(&str, &str, Option<Words>, Option<Words>, char, Role)] = &[
    (
        "agent-claude",
        "default",
        Some(Words::AiAgentModeAskFirst),
        Some(Words::AiAgentModeAskFirstHelp),
        icons::QUESTION,
        Role::Ask,
    ),
    (
        "agent-claude",
        "acceptEdits",
        Some(Words::AiAgentModeAcceptEdits),
        Some(Words::AiAgentModeAcceptEditsHelp),
        icons::PENCIL,
        Role::Other,
    ),
    (
        "agent-claude",
        "plan",
        Some(Words::AiAgentModePlan),
        Some(Words::AiAgentModePlanHelp),
        icons::LIST_CHECKS,
        Role::Other,
    ),
    (
        "agent-claude",
        "auto",
        Some(Words::AiAgentModeAuto),
        Some(Words::AiAgentModeAutoHelp),
        icons::SPARKLE,
        Role::Auto,
    ),
    (
        "agent-claude",
        "bypassPermissions",
        Some(Words::AiAgentModeFullAccess),
        Some(Words::AiAgentModeFullAccessHelp),
        icons::WARNING,
        Role::Full,
    ),
    (
        "agent-codex",
        "ask",
        Some(Words::AiAgentModeAskApproval),
        Some(Words::AiAgentModeAskApprovalHelp),
        icons::QUESTION,
        Role::Ask,
    ),
    (
        "agent-codex",
        "auto-review",
        Some(Words::AiAgentModeApproveForMe),
        Some(Words::AiAgentModeApproveForMeHelp),
        icons::ROBOT,
        Role::Auto,
    ),
    (
        "agent-codex",
        "full-access",
        Some(Words::AiAgentModeFullAccess),
        Some(Words::AiAgentModeCodexFullAccessHelp),
        icons::WARNING,
        Role::Full,
    ),
    (
        "agent-opencode",
        "build",
        Some(Words::AiAgentModeBuild),
        Some(Words::AiAgentModeBuildHelp),
        icons::WRENCH,
        Role::Ask,
    ),
    (
        "agent-opencode",
        "plan",
        Some(Words::AiAgentModePlan),
        Some(Words::AiAgentModeOpenCodePlanHelp),
        icons::LIST_CHECKS,
        Role::Other,
    ),
    // Devin over ACP (`devin acp`, D51). It has no mode that asks before
    // an edit: *Code* (`accept-edits`) is what it starts in and what
    // *Manual* means for it. Its `ask` only answers questions and never
    // edits, so it is not *Ask first*: it keeps Devin's own name and line.
    // Koru names the rest and Devin's own line explains them, but for
    // `bypass`, whose *Runs everything without asking.* is exactly it.
    (
        "agent-devin",
        "accept-edits",
        Some(Words::AiAgentModeAcceptEdits),
        None,
        icons::PENCIL,
        Role::Other,
    ),
    (
        "agent-devin",
        "smart",
        Some(Words::AiAgentModeAuto),
        None,
        icons::SPARKLE,
        Role::Auto,
    ),
    ("agent-devin", "ask", None, None, icons::FADERS, Role::Other),
    (
        "agent-devin",
        "plan",
        Some(Words::AiAgentModePlan),
        None,
        icons::LIST_CHECKS,
        Role::Other,
    ),
    (
        "agent-devin",
        "bypass",
        Some(Words::AiAgentModeFullAccess),
        Some(Words::AiAgentModeFullAccessHelp),
        icons::WARNING,
        Role::Full,
    ),
];

/// **What a mode is for**, as far as a new chat's default needs to know:
/// the agent's asking mode, the one that approves safe actions on its own,
/// the one that runs everything without asking, or none of those (Claude's
/// *Accept edits* and *Plan*, OpenCode's *Plan*).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Ask,
    Auto,
    Full,
    Other,
}

/// **The ids other agents spell the same three roles with** — the known
/// aliases, compared without case and with `_` read as `-`. A mode in
/// [`KNOWN`] has its role from there; these are for an agent Koru has no
/// table for, so that a new agent offering `yolo` or `full-access` is not
/// started in it by *Manual*, and is by *Yolo*.
const ASK_ALIASES: &[&str] = &[
    "default",
    "ask",
    "manual",
    "read-only",
    "readonly",
    "suggest",
];
const AUTO_ALIASES: &[&str] = &["auto", "auto-review", "autoreview", "auto-approve"];
const FULL_ALIASES: &[&str] = &[
    "bypasspermissions",
    "bypass-permissions",
    "full-access",
    "fullaccess",
    "yolo",
    "danger-full-access",
    "dangerously-skip-permissions",
];

/// `id`'s role for `agent`: its row in [`KNOWN`], else a known alias, else
/// [`Role::Other`].
pub(crate) fn role_of(agent: &AiProviderId, id: &str) -> Role {
    if let Some(known) = KNOWN
        .iter()
        .find(|known| known.0 == agent.as_str() && known.1 == id)
    {
        return known.5;
    }
    let spelled = id.to_ascii_lowercase().replace('_', "-");
    let spelled = spelled.as_str();
    if FULL_ALIASES.contains(&spelled) {
        Role::Full
    } else if AUTO_ALIASES.contains(&spelled) {
        Role::Auto
    } else if ASK_ALIASES.contains(&spelled) {
        Role::Ask
    } else {
        Role::Other
    }
}

/// Whether `id` is `agent`'s *Full access*: drawn in the warning color and
/// never stepped to by Shift+Tab.
pub(crate) fn is_full_access(agent: &AiProviderId, id: &str) -> bool {
    role_of(agent, id) == Role::Full
}

/// **The permission a new chat starts in**, as the person set it on *Agent
/// providers*. Each agent's own modes are what a chat runs in;
/// [`default_mode`] maps this onto them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NewChatPermission {
    /// The agent's asking mode: Claude's *Ask first*, Codex's *Ask for
    /// approval*, OpenCode's *Build*.
    #[default]
    Manual,
    /// The mode that approves safe actions on its own: Claude's *Auto*,
    /// Codex's *Approve for me*. An agent with none starts in *Manual*.
    Auto,
    /// The mode that runs everything without asking: Claude's
    /// `bypassPermissions` where it offers it, Codex's *Full access*. An
    /// agent that offers none starts in *Manual*.
    Yolo,
}

/// Whether `agent` is one of the **supported agents** — it offers a mode
/// that approves safe actions on its own — and so starts from the first of
/// the two defaults rather than the second.
pub(crate) fn has_auto_mode(agent: &AiProviderId, listed: &ListedModes) -> bool {
    listed
        .modes
        .iter()
        .any(|mode| role_of(agent, &mode.id) == Role::Auto)
}

/// **The mode a new chat with `agent` starts in** — the one tested place the
/// two default permissions meet an agent's own modes (D39).
///
/// An agent with an auto mode takes `supported`, any other `other` (where
/// *Auto* means *Manual*: it has nothing to approve with). Then:
///
/// - *Manual* is the agent's asking mode; failing one, the mode it reports
///   it starts in, unless that runs everything; failing that, its first
///   mode that does not.
/// - *Auto* is its auto mode, and *Manual* without one.
/// - *Yolo* is its full-access mode **only where it offers one**, and
///   *Manual* otherwise — never a mode the agent did not list, which the
///   adapter would refuse.
///
/// `None` only for an agent that lists no modes at all, whose chat has no
/// mode to start in.
pub(crate) fn default_mode(
    agent: &AiProviderId,
    listed: &ListedModes,
    supported: NewChatPermission,
    other: NewChatPermission,
) -> Option<String> {
    let first = |role: Role| {
        listed
            .modes
            .iter()
            .find(|mode| role_of(agent, &mode.id) == role)
            .map(|mode| mode.id.clone())
    };
    let manual = || {
        first(Role::Ask)
            .or_else(|| {
                listed
                    .default
                    .clone()
                    .filter(|id| listed.modes.iter().any(|mode| &mode.id == id))
                    .filter(|id| role_of(agent, id) != Role::Full)
            })
            .or_else(|| {
                listed
                    .modes
                    .iter()
                    .find(|mode| role_of(agent, &mode.id) != Role::Full)
                    .map(|mode| mode.id.clone())
            })
    };
    let permission = if has_auto_mode(agent, listed) {
        supported
    } else {
        other
    };
    match permission {
        NewChatPermission::Manual => manual(),
        NewChatPermission::Auto => first(Role::Auto).or_else(manual),
        NewChatPermission::Yolo => first(Role::Full).or_else(manual),
    }
}

/// `agent`'s modes as rows, in the agent's order: Koru's words for the ones
/// it knows, the agent's own name and description, and a neutral icon, for
/// the rest.
pub(crate) fn rows(
    lang: koru_i18n::Lang,
    agent: &AiProviderId,
    listed: &ListedModes,
) -> Vec<ModeRow> {
    listed
        .modes
        .iter()
        .map(|mode| {
            match KNOWN
                .iter()
                .find(|known| known.0 == agent.as_str() && known.1 == mode.id)
            {
                // Koru's words where it has them, the agent's own where it
                // does not (Devin's, D51); the role is Koru's either way.
                Some((_, _, title, help, icon, role)) => ModeRow {
                    id: mode.id.clone(),
                    title: title.map_or_else(
                        || mode.name.clone().unwrap_or_else(|| mode.id.clone()),
                        |title| t(lang, title).to_owned(),
                    ),
                    help: help
                        .map(|help| t(lang, help).to_owned())
                        .or_else(|| mode.description.clone()),
                    icon: *icon,
                    full_access: *role == Role::Full,
                },
                // An agent Koru has no words for keeps its own; one of the
                // known full-access spellings is still drawn and stepped as
                // *Full access*.
                None => ModeRow {
                    id: mode.id.clone(),
                    title: mode.name.clone().unwrap_or_else(|| mode.id.clone()),
                    help: mode.description.clone(),
                    icon: icons::FADERS,
                    full_access: is_full_access(agent, &mode.id),
                },
            }
        })
        .collect()
}

/// The mode after `current` among the ones Shift+Tab steps through — every
/// mode but *Full access* — wrapping at the end. From *Full access*, or a
/// mode no longer listed, the first.
pub(crate) fn next_mode<'a>(rows: &'a [ModeRow], current: Option<&str>) -> Option<&'a ModeRow> {
    let steps: Vec<&ModeRow> = rows.iter().filter(|row| !row.full_access).collect();
    let at = current.and_then(|current| steps.iter().position(|row| row.id == current));
    match at {
        Some(at) => steps.get((at + 1) % steps.len()).copied(),
        None => steps.first().copied(),
    }
}

/// How long after an agent's listing came without modes it is asked again.
const MODES_ASK_AGAIN: Duration = Duration::from_secs(2);
/// How many times, at most, per agent: the probe that reads them takes
/// seconds, up to about fifteen for the slowest agent, and an agent that
/// offers none is not asked forever.
const MODES_ASKS: u8 = 8;

impl crate::ChatSurface {
    /// The agent of the chat on screen, for an agent chat.
    fn shown_agent(&self) -> Option<&AiProviderId> {
        let provider = &self.transcript.as_ref()?.chat.current_model.provider;
        is_agent_provider(provider).then_some(provider)
    }

    /// **An agent's modes can come after its models.** The session host
    /// keeps an agent's modes in memory only, read by its probe's session
    /// start, while its models come from the list it kept on disk at once.
    /// A listing answered before that probe — the one asked as the window
    /// opens, against a host that has just started — carries no modes, and
    /// the chip was missing until the model picker happened to ask again.
    /// So while an agent chat is shown and its agent's modes are not known,
    /// its listing is asked again every [`MODES_ASK_AGAIN`], at most
    /// [`MODES_ASKS`] times; the host answers from memory.
    pub(crate) fn follow_agent_modes(&mut self, now: Instant) {
        let Some(agent) = self.modes_owed() else {
            return;
        };
        if self
            .modes_asked
            .get(&agent)
            .is_some_and(|(_, last)| now.duration_since(*last) < MODES_ASK_AGAIN)
        {
            return;
        }
        let asked = self.modes_asked.entry(agent.clone()).or_insert((0, now));
        *asked = (asked.0 + 1, now);
        self.ask_agent_models(agent);
    }

    /// When [`Self::follow_agent_modes`] asks next, if it will.
    pub(crate) fn modes_deadline(&self) -> Option<Instant> {
        let agent = self.modes_owed()?;
        Some(
            self.modes_asked
                .get(&agent)
                .map_or_else(Instant::now, |(_, last)| *last + MODES_ASK_AGAIN),
        )
    }

    /// The shown chat's agent, while its modes are owed: not known, no ask
    /// out for them, and asks left.
    fn modes_owed(&self) -> Option<AiProviderId> {
        let agent = self.shown_agent()?;
        let owed = !self.agent_modes.contains_key(agent)
            && !self.agent_models_asked.contains(agent)
            && self
                .modes_asked
                .get(agent)
                .is_none_or(|(count, _)| *count < MODES_ASKS);
        owed.then(|| agent.clone())
    }

    /// The chat's agent and its listed modes: an agent chat whose agent
    /// lists any. `None` for every other chat.
    fn chat_modes(&self) -> Option<(&AiProviderId, &ListedModes)> {
        let provider = &self.transcript.as_ref()?.chat.current_model.provider;
        if !is_agent_provider(provider) {
            return None;
        }
        let listed = self.agent_modes.get(provider)?;
        (!listed.modes.is_empty()).then_some((provider, listed))
    }

    /// The chat's modes as the picker shows them; empty when it has none.
    pub(crate) fn approval_mode_rows(&self) -> Vec<ModeRow> {
        self.chat_modes()
            .map(|(agent, listed)| rows(self.lang, agent, listed))
            .unwrap_or_default()
    }

    /// The mode the chat's next message runs in: the one it chose when the
    /// agent still lists it, else the agent's default, else its first.
    pub(crate) fn current_approval_mode(&self) -> Option<String> {
        let (_, listed) = self.chat_modes()?;
        let listed_id = |id: &str| listed.modes.iter().any(|mode| mode.id == id);
        self.transcript
            .as_ref()
            .and_then(|transcript| transcript.chat.agent_mode.clone())
            .filter(|id| listed_id(id))
            .or_else(|| listed.default.clone().filter(|id| listed_id(id)))
            .or_else(|| listed.modes.first().map(|mode| mode.id.clone()))
    }

    fn current_approval_row(&self) -> Option<ModeRow> {
        let current = self.current_approval_mode()?;
        self.approval_mode_rows()
            .into_iter()
            .find(|row| row.id == current)
    }

    /// **The mode a new chat with `agent` starts in**: the person's default
    /// permission mapped onto the agent's own modes ([`default_mode`]) —
    /// not the mode it was last used in. `None` until the agent has listed
    /// its modes; the chat takes its default when they arrive
    /// ([`Self::adopt_default_mode`]).
    pub(crate) fn default_approval_mode(&self, agent: &AiProviderId) -> Option<String> {
        let listed = self.agent_modes.get(agent)?;
        default_mode(
            agent,
            listed,
            self.agent_defaults.permission,
            self.agent_defaults.permission_other,
        )
    }

    /// A new chat not made yet whose agent had not listed its modes when it
    /// was begun takes its default now that they have arrived — unless a
    /// mode was chosen on it meanwhile.
    pub(crate) fn adopt_default_mode(&mut self, agent: &AiProviderId) {
        if !self.chat_unmade() {
            return;
        }
        let mode = self.default_approval_mode(agent);
        if let Some(transcript) = self.transcript.as_mut()
            && transcript.chat.current_model.provider == *agent
            && transcript.chat.agent_mode.is_none()
        {
            transcript.chat.agent_mode = mode;
        }
    }

    /// The chip, left of the model chip where an API chat has its
    /// Manual/Automatic chip: an agent chat whose agent lists modes.
    pub(crate) fn approval_mode_button(&self) -> Option<Rect> {
        self.chat_modes()?;
        let toolbar = self.composer_toolbar_rect();
        let model = self.chat_model_button();
        let width = self
            .approval_mode_chip_w
            .clamp(72.0, (toolbar.w * 0.4).max(72.0));
        Some(Rect::new(
            (model.x - 6.0 - width).max(toolbar.x),
            toolbar.y,
            width.min((model.x - toolbar.x - 6.0).max(1.0)),
            toolbar.h,
        ))
    }

    /// The open list: above the composer, its right edge on the chip's.
    pub(crate) fn approval_mode_panel(&self) -> Option<Rect> {
        if !self.approval_mode_open {
            return None;
        }
        let chip = self.approval_mode_button()?;
        let count = self.approval_mode_rows().len();
        let width = self.composer_hit_rect.w.min(340.0).max(1.0);
        let height = count as f32 * MODE_ROW_H + PANEL_PAD * 2.0;
        Some(self.above_composer(chip.x + chip.w - width, width, height))
    }

    fn approval_mode_row_rect(&self, panel: Rect, index: usize) -> Rect {
        Rect::new(
            panel.x + PANEL_PAD,
            panel.y + PANEL_PAD + index as f32 * MODE_ROW_H,
            panel.w - PANEL_PAD * 2.0,
            MODE_ROW_H,
        )
    }

    fn approval_mode_row_at(&self, x: f32, y: f32) -> Option<usize> {
        let panel = self.approval_mode_panel()?;
        (0..self.approval_mode_rows().len())
            .find(|index| self.approval_mode_row_rect(panel, *index).contains(x, y))
    }

    /// The hover part for redraws: the chip, and each row of the open list.
    pub(crate) fn approval_mode_hover_at(&self, x: f32, y: f32) -> Option<(u8, usize)> {
        if let Some(index) = self.approval_mode_row_at(x, y) {
            return Some((17, index));
        }
        self.approval_mode_button()
            .filter(|chip| chip.contains(x, y))
            .map(|_| (5, 13))
    }

    pub(crate) fn close_approval_modes(&mut self) -> bool {
        let was = self.approval_mode_open;
        self.approval_mode_open = false;
        self.approval_mode_focus = None;
        was
    }

    /// **Choose a mode for the chat's next message**. The chip names it; a
    /// note beside the chip speaks only while a reply runs, to say it
    /// applies to the next message (D65).
    pub(crate) fn choose_approval_mode(&mut self, id: &str) -> bool {
        let Some(row) = self
            .approval_mode_rows()
            .into_iter()
            .find(|row| row.id == id)
        else {
            return false;
        };
        let Some(chat) = self
            .transcript
            .as_ref()
            .map(|transcript| transcript.chat.id)
        else {
            return false;
        };
        self.close_approval_modes();
        if self.current_approval_mode().as_deref() != Some(id) {
            // Shown at once; the service's answer confirms it.
            if let Some(transcript) = &mut self.transcript {
                transcript.chat.agent_mode = Some(id.to_owned());
            }
            self.send_command(Command::UpdateAgentSettings(
                chat,
                UpdateChatAgentSettingsRequest {
                    mode: Some(id.to_owned()),
                    fast: None,
                },
            ));
        }
        if self.attempt.is_some() {
            self.announce_next_message(&row);
        }
        true
    }

    /// The short note beside the chip while a reply runs: the mode applies
    /// to the next message. With no reply running there is none — the
    /// chip's label already names the mode (D65).
    fn announce_next_message(&mut self, row: &ModeRow) {
        let Some(chip) = self.approval_mode_button() else {
            return;
        };
        let label = fill(t(self.lang, Words::AiAgentModeNextMessage), &[&row.title]);
        self.copy_feedback.show_transient(
            TooltipContent::new(NOTE_KEY, chip, label),
            Instant::now(),
            NOTE_HOLD,
        );
    }

    /// **Shift+Tab**: the next mode, skipping *Full access*. `false` for a
    /// chat with no modes, so the key goes on as it would have.
    pub(crate) fn cycle_approval_mode(&mut self) -> bool {
        let rows = self.approval_mode_rows();
        let current = self.current_approval_mode();
        let Some(next) = next_mode(&rows, current.as_deref()).cloned() else {
            return false;
        };
        self.choose_approval_mode(&next.id)
    }

    /// The open list's keys: Up and Down move, Enter chooses, Escape closes.
    /// `None` when the list is shut or the key is not one of them.
    pub(crate) fn approval_modes_key(&mut self, key: &Key, mods: ModifiersState) -> Option<bool> {
        if !self.approval_mode_open || !mods.is_empty() {
            return None;
        }
        let rows = self.approval_mode_rows();
        let current = self.current_approval_mode();
        let focused = self
            .approval_mode_focus
            .or_else(|| {
                rows.iter()
                    .position(|row| Some(&row.id) == current.as_ref())
            })
            .unwrap_or(0);
        match key {
            Key::Named(NamedKey::Escape) => Some(self.close_approval_modes()),
            Key::Named(NamedKey::ArrowDown) if !rows.is_empty() => {
                self.approval_mode_focus = Some((focused + 1) % rows.len());
                Some(true)
            }
            Key::Named(NamedKey::ArrowUp) if !rows.is_empty() => {
                self.approval_mode_focus = Some((focused + rows.len() - 1) % rows.len());
                Some(true)
            }
            Key::Named(NamedKey::Enter) => {
                let id = rows.get(focused).map(|row| row.id.clone())?;
                Some(self.choose_approval_mode(&id))
            }
            _ => None,
        }
    }

    /// A press: the chip opens and shuts the list, a row chooses its mode,
    /// and a press anywhere else shuts the list and goes on (`None`).
    pub(crate) fn approval_mode_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        if let Some(index) = self.approval_mode_row_at(x, y) {
            let id = self.approval_mode_rows().get(index)?.id.clone();
            return Some(self.choose_approval_mode(&id));
        }
        if self
            .approval_mode_button()
            .is_some_and(|chip| chip.contains(x, y))
        {
            let open = !self.approval_mode_open;
            self.history_open = false;
            self.tool_access_open = false;
            self.tool_mode_open = false;
            if self.chat_model_open {
                self.close_picker();
            }
            self.approval_mode_open = open;
            self.approval_mode_focus = None;
            return Some(true);
        }
        if self.approval_mode_panel().is_some() {
            self.close_approval_modes();
        }
        None
    }

    /// The chip: the mode's icon and name, and a caret, as a ghost button.
    pub(crate) fn draw_approval_mode_chip(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(row) = self.current_approval_row() else {
            return;
        };
        let p = theme::p();
        let icon = self.engine.icon_layout(row.icon, ActionButton::ICON_SIZE);
        let caret = self
            .engine
            .icon_layout(icons::CARET_DOWN, ActionButton::ICON_SIZE);
        let label = self.engine.simple_layout(&row.title, role::row());
        self.approval_mode_chip_w = ActionButton::PAD_X * 2.0
            + icon.width()
            + ActionButton::GAP
            + label.width()
            + ActionButton::GAP
            + caret.width();
        let Some(chip) = self.approval_mode_button() else {
            return;
        };
        let hovered = chip.contains(self.pointer.0, self.pointer.1);
        if hovered || self.approval_mode_open {
            scene::fill(
                canvas,
                root,
                p.hover_tint,
                &scene::rrect(chip.x, chip.y, chip.w, chip.h, ActionButton::RADIUS),
            );
        }
        let colour = if row.full_access { p.notice_fg } else { p.text };
        let mut x = chip.x + ActionButton::PAD_X;
        scene::draw_layout_first_line(
            canvas,
            root,
            &icon,
            x,
            chip.y + (chip.h - icon.height()) * 0.5,
            Some(colour),
        );
        x += icon.width() + ActionButton::GAP;
        let room = (chip.x + chip.w - ActionButton::PAD_X - caret.width() - ActionButton::GAP - x)
            .max(1.0);
        let shown = self.engine.elided(&row.title, role::row(), room);
        let label = self.engine.simple_layout(&shown, role::row());
        scene::draw_layout_first_line(
            canvas,
            root,
            &label,
            x,
            chip.y + (chip.h - label.height()) * 0.5,
            Some(colour),
        );
        scene::draw_layout_first_line(
            canvas,
            root,
            &caret,
            chip.x + chip.w - ActionButton::PAD_X - caret.width(),
            chip.y + (chip.h - caret.height()) * 0.5,
            Some(p.text_dim),
        );
    }

    /// The open list: each mode's icon, title and line, a check on the
    /// current one, *Full access* in the warning color.
    pub(crate) fn draw_approval_mode_picker(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(panel) = self.approval_mode_panel() else {
            return;
        };
        scene::floating_panel(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(8.0),
        );
        let p = theme::p();
        let rows = self.approval_mode_rows();
        let current = self.current_approval_mode();
        let check = self
            .engine
            .icon_layout(icons::CHECK, ActionButton::ICON_SIZE);
        for (index, row) in rows.iter().enumerate() {
            let rect = self.approval_mode_row_rect(panel, index);
            let focused = self.approval_mode_focus == Some(index);
            if focused || rect.contains(self.pointer.0, self.pointer.1) {
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint,
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
                );
            }
            let colour = if row.full_access { p.notice_fg } else { p.text };
            let icon = self.engine.icon_layout(row.icon, ActionButton::ICON_SIZE);
            scene::draw_layout_first_line(
                canvas,
                root,
                &icon,
                rect.x + 10.0,
                rect.y + 8.0,
                Some(colour),
            );
            let text_x = rect.x + 34.0;
            let room = (rect.x + rect.w - 30.0 - text_x).max(1.0);
            let title = self.engine.elided(&row.title, role::row(), room);
            let title = self.engine.simple_layout(&title, role::row());
            scene::draw_layout_first_line(canvas, root, &title, text_x, rect.y + 5.0, Some(colour));
            if let Some(help) = &row.help {
                let help = self.engine.elided(help, role::meta(), room);
                let help = self.engine.simple_layout(&help, role::meta());
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &help,
                    text_x,
                    rect.y + 5.0 + title.height() + 2.0,
                    Some(p.text_dim),
                );
            }
            if current.as_ref() == Some(&row.id) {
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &check,
                    rect.x + rect.w - 10.0 - check.width(),
                    rect.y + (rect.h - check.height()) * 0.5,
                    Some(p.accent),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
