// SPDX-License-Identifier: MPL-2.0

//! **What an agent can be told to do by name** (protocol v21, koru-ai
//! D121): the entries a chat's composer offers as it is typed — Codex's
//! skills under `$` today.
//!
//! One shape for every agent: a name, a line about it, a name to show when
//! the agent gives one, and where it comes from ([`AgentCommandKind`]). An
//! ACP agent's `available_commands_update` (`{name, description, input}`)
//! fits the same shape as a later kind with its input hint, for a picker
//! under `/`; nothing builds that yet.
//!
//! The list is **discovered**, never read from disk by Koru: the agent lists
//! it for a folder (`discover_provider_commands`), the adapter shapes and
//! bounds it, and a picked skill goes back **by name** with the person's
//! input ([`PendingInput::skills`](crate::PendingInput::skills)). Where a
//! skill lives on disk stays in the adapter, which resolves the name from
//! the agent's own last listing.
//!
//! Names and words are the agent's and the person's: `Debug` names only the
//! shape.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most entries one listing keeps, in its order; any after are dropped.
pub const MAX_AGENT_COMMANDS: usize = 512;
/// The most bytes of a name. A longer one is not offered: a name is sent
/// back exactly, so it is never cut.
pub const AGENT_COMMAND_NAME_BYTES: usize = 128;
/// The most characters of a description or a display name; longer is cut.
pub const AGENT_COMMAND_TEXT_CHARS: usize = 300;
/// The most skills one input may name.
pub const MAX_INPUT_SKILLS: usize = 16;

/// **Where an entry comes from**: the folder's own, the person's, what the
/// agent ships, what an administrator set. A picker lists the first two
/// groups first and the last two together ([`AgentCommandKind::group`]).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentCommandKind {
    /// An ACP `available_commands_update` entry invoked by `/name`.
    Command,
    /// A skill of the folder the agent works in (Codex's `repo` scope).
    RepoSkill,
    /// A skill of the person's own (`user`).
    UserSkill,
    /// A skill the agent ships with (`system`).
    SystemSkill,
    /// A skill an administrator installed (`admin`).
    AdminSkill,
}

impl AgentCommandKind {
    /// Whether it is invoked as a skill: by `$name` in the text and the
    /// skill named beside the input.
    pub fn is_skill(self) -> bool {
        matches!(
            self,
            Self::RepoSkill | Self::UserSkill | Self::SystemSkill | Self::AdminSkill
        )
    }

    /// **The group a picker lists it in**: the folder's, then the person's,
    /// then the agent's own and an administrator's as one — by name within
    /// each (the user's order, 2026-11-03).
    pub fn group(self) -> u8 {
        match self {
            Self::Command => 0,
            Self::RepoSkill => 0,
            Self::UserSkill => 1,
            Self::SystemSkill | Self::AdminSkill => 2,
        }
    }
}

/// **One entry an agent offers.** `name` is what the person's message says
/// and what goes back; `description` is one line about it, empty when the
/// agent gave none; `display_name` is the agent's own name to show, when it
/// gives one.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub kind: AgentCommandKind,
    /// What the entry takes after its name, in the agent's words, when it
    /// says. No skill has one; it is the ACP command's `input.hint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hint: Option<String>,
}

impl std::fmt::Debug for AgentCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentCommand")
            .field("name_bytes", &self.name.len())
            .field("kind", &self.kind)
            .finish()
    }
}

/// Whether `name` may be offered and sent back: not empty, within
/// [`AGENT_COMMAND_NAME_BYTES`], with no whitespace or control character.
/// It is written into the person's message as one `$name` or `/name` token.
pub fn valid_command_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= AGENT_COMMAND_NAME_BYTES
        && !name
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
}

/// Whether the skills an input names may be sent: at most
/// [`MAX_INPUT_SKILLS`], each a valid name, none twice.
pub fn valid_input_skills(skills: &[String]) -> bool {
    skills.len() <= MAX_INPUT_SKILLS
        && skills.iter().all(|name| valid_command_name(name))
        && skills
            .iter()
            .enumerate()
            .all(|(index, name)| !skills[..index].contains(name))
}

/// `text` cut to [`AGENT_COMMAND_TEXT_CHARS`] on its first line, with no
/// control characters: what a description or a display name is kept as.
pub fn command_text(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    line.chars()
        .filter(|character| !character.is_control())
        .take(AGENT_COMMAND_TEXT_CHARS)
        .collect()
}

/// **A listing in the order a picker shows it**, bounded: each entry with a
/// valid name, ordered by its kind's [group](AgentCommandKind::group) and
/// then by name, each name kept once — the first in that order, so a
/// folder's skill stands for a person's of the same name — and at most
/// [`MAX_AGENT_COMMANDS`].
pub fn ordered_commands(mut commands: Vec<AgentCommand>) -> Vec<AgentCommand> {
    commands.retain(|command| valid_command_name(&command.name));
    commands.sort_by(|left, right| {
        left.kind
            .group()
            .cmp(&right.kind.group())
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.kind.cmp(&right.kind))
    });
    let mut kept: Vec<AgentCommand> = Vec::with_capacity(commands.len());
    for command in commands {
        if kept.iter().all(|known| known.name != command.name) {
            kept.push(command);
        }
    }
    kept.truncate(MAX_AGENT_COMMANDS);
    kept
}
