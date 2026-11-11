// SPDX-License-Identifier: MPL-2.0

//! **The approval modes' vocabulary** (koru-ai D7, D39, D51): an agent's
//! own modes worded in Koru's words where Koru knows them, what each is for,
//! the mode a new conversation starts in under the person's default
//! permission, and the order Shift+Tab steps through them.
//!
//! **The agent's own modes, in Koru's words.** A host hands over the modes an
//! agent lists ([`ModeOffer`]); the ones Koru knows — Claude Code's,
//! Codex's three, OpenCode's two, Devin's — are worded here, and any other
//! shows the agent's own name and description with a neutral icon. The table
//! is data about the agents' own mode ids, so any host words them the same.

use koru_draw::icons;
use koru_i18n::{Key as Words, Lang, t};

pub mod elements;
pub mod tokens;
pub mod view;

pub use view::ModeList;

/// One row of the list: an icon, a title and a line under it.
pub const MODE_ROW_H: f32 = 46.0;

/// **One mode an agent lists**, as its host has it: the agent's id for it,
/// and the agent's own name and line, if it gave them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModeOffer<'a> {
    pub id: &'a str,
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
}

/// One mode as the list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeRow {
    pub id: String,
    pub title: String,
    pub help: Option<String>,
    pub icon: char,
    /// *Full access*: drawn in the warning color, never stepped to by
    /// Shift+Tab, and a new conversation's only by a *Yolo* default.
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
pub enum Role {
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
pub fn role_of(agent: &str, id: &str) -> Role {
    if let Some(known) = KNOWN.iter().find(|known| known.0 == agent && known.1 == id) {
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
pub fn is_full_access(agent: &str, id: &str) -> bool {
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
pub fn has_auto_mode(agent: &str, modes: &[ModeOffer<'_>]) -> bool {
    modes
        .iter()
        .any(|mode| role_of(agent, mode.id) == Role::Auto)
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
pub fn default_mode(
    agent: &str,
    modes: &[ModeOffer<'_>],
    default: Option<&str>,
    supported: NewChatPermission,
    other: NewChatPermission,
) -> Option<String> {
    let first = |role: Role| {
        modes
            .iter()
            .find(|mode| role_of(agent, mode.id) == role)
            .map(|mode| mode.id.to_owned())
    };
    let manual = || {
        first(Role::Ask)
            .or_else(|| {
                default
                    .filter(|id| modes.iter().any(|mode| mode.id == *id))
                    .filter(|id| role_of(agent, id) != Role::Full)
                    .map(str::to_owned)
            })
            .or_else(|| {
                modes
                    .iter()
                    .find(|mode| role_of(agent, mode.id) != Role::Full)
                    .map(|mode| mode.id.to_owned())
            })
    };
    let permission = if has_auto_mode(agent, modes) {
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
pub fn rows(lang: Lang, agent: &str, modes: &[ModeOffer<'_>]) -> Vec<ModeRow> {
    modes
        .iter()
        .map(|mode| {
            match KNOWN
                .iter()
                .find(|known| known.0 == agent && known.1 == mode.id)
            {
                // Koru's words where it has them, the agent's own where it
                // does not (Devin's, D51); the role is Koru's either way.
                Some((_, _, title, help, icon, role)) => ModeRow {
                    id: mode.id.to_owned(),
                    title: title.map_or_else(
                        || mode.name.unwrap_or(mode.id).to_owned(),
                        |title| t(lang, title).to_owned(),
                    ),
                    help: help
                        .map(|help| t(lang, help).to_owned())
                        .or_else(|| mode.description.map(str::to_owned)),
                    icon: *icon,
                    full_access: *role == Role::Full,
                },
                // An agent Koru has no words for keeps its own; one of the
                // known full-access spellings is still drawn and stepped as
                // *Full access*.
                None => ModeRow {
                    id: mode.id.to_owned(),
                    title: mode.name.unwrap_or(mode.id).to_owned(),
                    help: mode.description.map(str::to_owned),
                    icon: icons::FADERS,
                    full_access: is_full_access(agent, mode.id),
                },
            }
        })
        .collect()
}

/// The mode after `current` among the ones Shift+Tab steps through — every
/// mode but *Full access* — wrapping at the end. From *Full access*, or a
/// mode no longer listed, the first.
pub fn next_mode<'a>(rows: &'a [ModeRow], current: Option<&str>) -> Option<&'a ModeRow> {
    let steps: Vec<&ModeRow> = rows.iter().filter(|row| !row.full_access).collect();
    let at = current.and_then(|current| steps.iter().position(|row| row.id == current));
    match at {
        Some(at) => steps.get((at + 1) % steps.len()).copied(),
        None => steps.first().copied(),
    }
}
