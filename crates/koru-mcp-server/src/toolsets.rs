// SPDX-License-Identifier: MPL-2.0

//! The published tools in named groups a bridge can be started with.
//!
//! Every agent reads the whole `tools/list` reply into its context at the start
//! of a session, about 20 KB with every tool on. An agent that only works on
//! documents pays for tasks, chats, files and history it never calls, so the
//! bridge takes `--toolsets docs,tasks` and serves only those groups. **Absent,
//! every toolset is on**: the flag only ever narrows.
//!
//! `core` — the documentation and the workspace list — is always on, because
//! no other tool can be called without them. Every tool is in exactly one
//! toolset; `every_published_tool_is_in_exactly_one_toolset` holds the table
//! below to the router. A tool from a toolset that is off is not listed, and a
//! call to it is an `invalid_params` error that names its toolset and the flag
//! that turns it on, rather than rmcp's bare "tool not found".
//!
//! Koru AI reads [`crate::workspace_tool_catalog`], not this bridge, and always
//! sees every tool.

use std::fmt;

use rmcp::model::ErrorData;
use serde_json::json;

use crate::CHAT_TOOL_NAMES;

/// One named group of tools.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Toolset {
    /// `get_documentation` and `list_workspaces`: always on.
    Core,
    /// Find, read, create and edit documents, images and extraction.
    Docs,
    /// Workspace files and a file's note.
    Files,
    /// Saved versions: save, list, compare and restore.
    Versions,
    /// The open-task list.
    Tasks,
    /// Koru AI chats, read-only.
    Chat,
}

impl Toolset {
    /// Every toolset, in the order they are listed and named.
    pub const ALL: [Toolset; 6] = [
        Toolset::Core,
        Toolset::Docs,
        Toolset::Files,
        Toolset::Versions,
        Toolset::Tasks,
        Toolset::Chat,
    ];

    /// The name `--toolsets` takes.
    pub fn name(self) -> &'static str {
        match self {
            Toolset::Core => "core",
            Toolset::Docs => "docs",
            Toolset::Files => "files",
            Toolset::Versions => "versions",
            Toolset::Tasks => "tasks",
            Toolset::Chat => "chat",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|set| set.name() == name)
    }

    /// The tools this toolset serves.
    pub fn tools(self) -> &'static [&'static str] {
        match self {
            Toolset::Core => &["get_documentation", "list_workspaces"],
            Toolset::Docs => &[
                "list_documents",
                "search_documents",
                "read_document",
                "create_document",
                "edit_document",
                "add_image",
                "extract_to_document",
            ],
            Toolset::Files => &[
                "list_files",
                "search_files",
                "get_file",
                "read_file",
                "ensure_file_note",
            ],
            Toolset::Versions => &[
                "save_document_version",
                "list_document_versions",
                "compare_document_versions",
                "restore_document_version",
            ],
            Toolset::Tasks => &["list_tasks"],
            Toolset::Chat => CHAT_TOOL_NAMES,
        }
    }

    /// The toolset `tool` is in, if it is a published tool.
    pub fn of(tool: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|set| set.tools().contains(&tool))
    }

    fn bit(self) -> u8 {
        1 << self as u8
    }
}

impl fmt::Display for Toolset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The toolsets one bridge serves. `core` is always among them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Toolsets {
    on: u8,
}

impl Default for Toolsets {
    fn default() -> Self {
        Self::all()
    }
}

impl Toolsets {
    /// Every toolset: what a bridge serves without `--toolsets`.
    pub fn all() -> Self {
        Self {
            on: Toolset::ALL.into_iter().fold(0, |on, set| on | set.bit()),
        }
    }

    /// `--toolsets`' value: comma-separated toolset names, `core` implied. An
    /// unknown or empty name is an error, never skipped — a mistyped name that
    /// was dropped would start a bridge without the tools its user asked for.
    pub fn parse(list: &str) -> Result<Self, String> {
        let mut on = Toolset::Core.bit();
        for name in list.split(',').map(str::trim) {
            if name.is_empty() {
                return Err(format!(
                    "--toolsets needs a comma-separated list of {}",
                    Self::names()
                ));
            }
            let set = Toolset::from_name(name).ok_or_else(|| {
                format!(
                    "unknown toolset {name:?}; the toolsets are {}",
                    Self::names()
                )
            })?;
            on |= set.bit();
        }
        Ok(Self { on })
    }

    fn names() -> String {
        Toolset::ALL.map(Toolset::name).join(", ")
    }

    pub fn contains(self, set: Toolset) -> bool {
        self.on & set.bit() != 0
    }

    pub fn is_all(self) -> bool {
        self == Self::all()
    }

    /// The toolsets that are on, in [`Toolset::ALL`]'s order.
    pub fn enabled(self) -> impl Iterator<Item = Toolset> {
        Toolset::ALL
            .into_iter()
            .filter(move |set| self.contains(*set))
    }

    /// The toolsets that are off.
    pub fn disabled(self) -> impl Iterator<Item = Toolset> {
        Toolset::ALL
            .into_iter()
            .filter(move |set| !self.contains(*set))
    }

    /// Why `tool` is not served, when its toolset is off. `None` for a tool
    /// this bridge serves and for a name that is no tool at all, which the
    /// router answers as it always has.
    pub fn refusal(self, tool: &str) -> Option<ErrorData> {
        let set = Toolset::of(tool).filter(|set| !self.contains(*set))?;
        let mut wanted: Vec<&str> = self
            .enabled()
            .filter(|on| *on != Toolset::Core)
            .map(Toolset::name)
            .collect();
        wanted.push(set.name());
        Some(ErrorData::invalid_params(
            format!(
                "{tool} is in the {set} toolset, which this Koru bridge was started without \
                 (toolsets on: {self}). Start it with --toolsets {}, or without --toolsets for \
                 every toolset.",
                wanted.join(",")
            ),
            Some(json!({
                "tool": tool,
                "toolset": set.name(),
                "enabled_toolsets": self.enabled().map(Toolset::name).collect::<Vec<_>>(),
            })),
        ))
    }

    /// What the server instructions and the contract's table of contents add
    /// when some toolsets are off; nothing when every one is on.
    pub fn note(self) -> Option<String> {
        if self.is_all() {
            return None;
        }
        let off: Vec<&str> = self.disabled().map(Toolset::name).collect();
        Some(format!(
            "This bridge serves the toolsets {self}; {} {} off, so their tools are not listed \
             and a section about them describes tools this bridge does not serve.",
            off.join(", "),
            if off.len() == 1 { "is" } else { "are" }
        ))
    }
}

impl fmt::Display for Toolsets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.enabled().map(Toolset::name).collect();
        f.write_str(&names.join(", "))
    }
}

/// Each toolset's share of the `tools/list` reply, in bytes: the sum of its
/// tools as each is published (compacted), plus one separator each. Set at
/// the size measured on 2026-11-02 plus about 10% (measured: core 697, docs
/// 12,396, files 2,266, versions 2,036, tasks 1,682, chat 1,369). Growth past
/// one is a decision to record in MCP.md's table, not a number to bump.
#[cfg(test)]
pub(crate) const BUDGETS: [(Toolset, usize); 6] = [
    (Toolset::Core, 770),
    (Toolset::Docs, 13_650),
    (Toolset::Files, 2_500),
    (Toolset::Versions, 2_250),
    (Toolset::Tasks, 1_860),
    (Toolset::Chat, 1_510),
];

/// What a `tools/list` reply line spends outside its tools: the JSON-RPC
/// envelope, `"tools":[…]`, and the result type and cache hints. 45 bytes
/// measured on 2026-11-02.
#[cfg(test)]
pub(crate) const REPLY_ENVELOPE_BYTES: usize = 100;

/// The whole reply with every toolset on: every toolset's budget and the
/// envelope.
#[cfg(test)]
pub(crate) const fn total_budget() -> usize {
    let mut total = REPLY_ENVELOPE_BYTES;
    let mut index = 0;
    while index < BUDGETS.len() {
        total += BUDGETS[index].1;
        index += 1;
    }
    total
}

#[cfg(test)]
mod tests;
