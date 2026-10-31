// SPDX-License-Identifier: MPL-2.0

//! *Manage documents over MCP*: the command that registers this
//! installation with the agent the person picks, shown on the row and put on
//! the clipboard by *Copy*.
//!
//! **Each agent's own `mcp add`**, because they differ: Claude Code, Codex,
//! Copilot, Grok and Antigravity take the bridge after `--`; Gemini and Qwen
//! Code take it without one (their parser refuses `--` there). Claude Code
//! and Gemini register for the current folder unless told `--scope user`, so
//! they are told — the bridge serves the workspace, not a project. Checked
//! against each CLI's `mcp add --help`, and Claude Code, Gemini and Qwen Code
//! by running it. *Other* is the bridge's own command line, for a host
//! configured by hand.
//!
//! **The bridge beside this executable**, as a chat's agent starts it
//! (`koru-session-host`'s `chat_bridge`): a nightly window hands out the
//! nightly bridge, never one found on `PATH`. `--root` goes with it when this
//! launch was given one, or the host would serve a different store from the
//! one this window shows.

use std::path::Path;

/// The bridge's file name — `koru_update::exe::MCP`, which this app does not
/// link.
const MCP_EXE: &str = if cfg!(windows) {
    "koru-mcp.exe"
} else {
    "koru-mcp"
};

/// The bridge beside this executable and the root this launch was given, the
/// two things a registration needs; [`command`] is them as one line.
pub(crate) fn bridge() -> (std::path::PathBuf, Option<std::path::PathBuf>) {
    let root = koru_workspace_host::cli::root_from_args()
        .map(|root| std::path::absolute(&root).unwrap_or(root));
    let exe = std::env::current_exe()
        .map(|exe| exe.with_file_name(MCP_EXE))
        .unwrap_or_else(|_| std::path::PathBuf::from(MCP_EXE));
    (exe, root)
}

/// The agent the bridge is being registered with.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Agent {
    ClaudeCode,
    Codex,
    Gemini,
    Copilot,
    Qwen,
    Grok,
    Antigravity,
    /// Any other host: the bridge's own command line, to configure by hand.
    Other,
}

impl Agent {
    /// In the order the row offers them.
    pub(crate) const ALL: [Agent; 8] = [
        Agent::ClaudeCode,
        Agent::Codex,
        Agent::Gemini,
        Agent::Copilot,
        Agent::Qwen,
        Agent::Grok,
        Agent::Antigravity,
        Agent::Other,
    ];

    /// The chip's label: the product's own name, which is not translated.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Gemini => "Gemini",
            Agent::Copilot => "Copilot",
            Agent::Qwen => "Qwen Code",
            Agent::Grok => "Grok",
            Agent::Antigravity => "Antigravity",
            Agent::Other => "Other",
        }
    }

    /// What goes before the bridge: the agent's program and its `mcp add`,
    /// or nothing for [`Agent::Other`].
    fn prefix(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude mcp add --scope user koru -- ",
            Agent::Codex => "codex mcp add koru -- ",
            Agent::Gemini => "gemini mcp add --scope user koru ",
            Agent::Copilot => "copilot mcp add koru -- ",
            Agent::Qwen => "qwen mcp add --scope user koru ",
            Agent::Grok => "grok mcp add koru -- ",
            Agent::Antigravity => "agy mcp add koru -- ",
            Agent::Other => "",
        }
    }
}

/// The command for `agent` on this installation, ready to paste.
pub(crate) fn command(agent: Agent) -> String {
    // Absolute, because the host that runs it starts in a directory of its own.
    let (bridge, root) = bridge();
    agent_line(agent, &bridge, root.as_deref())
}

/// `agent`'s registration of `<bridge> serve --stdio [--root <root>]`.
pub(super) fn agent_line(agent: Agent, bridge: &Path, root: Option<&Path>) -> String {
    format!("{}{}", agent.prefix(), command_line(bridge, root))
}

/// `<bridge> serve --stdio [--root <root>]`, each path quoted when it has a
/// space in it.
pub(super) fn command_line(bridge: &Path, root: Option<&Path>) -> String {
    let mut line = format!("{} serve --stdio", quoted(bridge));
    if let Some(root) = root {
        line.push_str(" --root ");
        line.push_str(&quoted(root));
    }
    line
}

fn quoted(path: &Path) -> String {
    let text = path.display().to_string();
    if text.contains(' ') {
        format!("\"{text}\"")
    } else {
        text
    }
}
