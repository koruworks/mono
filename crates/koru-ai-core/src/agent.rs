// SPDX-License-Identifier: MPL-2.0

//! What a local agent's turn adds to the chat's vocabulary.
//!
//! A local agent (Codex, Claude Code, OpenCode …) runs as a managed session
//! the desktop's session host owns; the AI worker drives it and translates
//! its neutral events into the chat's own. Two things cross that the chat's
//! workspace tools never had:
//!
//! - **a live description** of what the agent is doing — the label it gave a
//!   tool, the summary of a permission it asks for, the choices it offered.
//!   That is provider content: it is shown while the turn runs and it is
//!   **never stored, logged or measured** (decided 2026-09-24, AGENTUX D5-1);
//!   reopened, a row says only its kind, its outcome and its time;
//! - **a closed kind** for the durable row's `tool_name`, spelled `agent.*`,
//!   so history keeps what happened without keeping the agent's wording.

use serde::{Deserialize, Serialize};

use crate::{ToolErrorCode, ToolInvocationId};

/// The chat's model column for "the agent's own default": a model selection
/// must be non-empty, and an agent chosen without a model has none of its
/// own to name. The agent provider treats it as no explicit selection.
pub const AGENT_DEFAULT_MODEL: &str = "default";

/// The closed kinds an agent's activity row is filed under. Stored as the
/// row's `tool_name`; the surface maps each to localized copy.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AgentToolKind {
    /// A tool whose kind the agent has not said yet: every tool starts here
    /// and is reclassified when it completes.
    Tool,
    Command,
    FileChange,
    Search,
    Mcp,
    Other,
    /// A permission request, which is its own row: the agent's protocol
    /// ties it to no tool call.
    Approval,
}

impl AgentToolKind {
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Tool => "agent.tool",
            Self::Command => "agent.command",
            Self::FileChange => "agent.file_change",
            Self::Search => "agent.search",
            Self::Mcp => "agent.mcp",
            Self::Other => "agent.other",
            Self::Approval => "agent.approval",
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "agent.tool" => Self::Tool,
            "agent.command" => Self::Command,
            "agent.file_change" => Self::FileChange,
            "agent.search" => Self::Search,
            "agent.mcp" => Self::Mcp,
            "agent.other" => Self::Other,
            "agent.approval" => Self::Approval,
            _ => return None,
        })
    }

    /// Whether a `tool_name` names an agent's row rather than a workspace
    /// tool's. The worker branches on this to route an approval to the
    /// agent instead of resuming the workspace tool loop.
    pub fn is_agent_token(token: &str) -> bool {
        Self::parse(token).is_some()
    }
}

/// What choosing an offered option means, as the agent classified it. The
/// surface focuses a rejection first and never picks between two options of
/// one intent on the person's behalf.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ChoiceIntent {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    Unknown,
}

/// One option an agent offered for a permission request. `id` is opaque and
/// scoped to its request; `label` is the agent's own wording, bounded and
/// live only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OfferedChoice {
    pub id: String,
    pub intent: ChoiceIntent,
    pub label: String,
}

/// The live half of an agent's activity row: shown while the turn runs,
/// `None` on every repository read and after the turn ends.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AgentActivityLive {
    /// The choices offered for a pending permission request; empty once
    /// answered. The agent's wording for the row — its label, a permission's
    /// summary, the call's input and output — is on the row itself, stored
    /// (schema 8); only the choices, which a person answers once, are live.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<OfferedChoice>,
}

/// The longest label or summary that crosses to the surface. Anything
/// longer is cut at a character boundary; the agent's wording is a courtesy,
/// not a record.
pub const LIVE_TEXT_LIMIT: usize = 512;

/// The most of a tool call's input or output the chat keeps, in characters
/// — the same bound the session protocol applies before it crosses, applied
/// again here so a row never carries more than its column admits.
pub const AGENT_CONTENT_CHARS: usize = 4_000;

/// `text`, trimmed and cut to [`AGENT_CONTENT_CHARS`] characters; `None`
/// when nothing readable is left.
pub fn bound_agent_content(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.chars().take(AGENT_CONTENT_CHARS).collect())
}

/// Cut live text to [`LIVE_TEXT_LIMIT`] on a character boundary.
pub fn bound_live_text(text: &str) -> String {
    if text.len() <= LIVE_TEXT_LIMIT {
        return text.to_owned();
    }
    let mut end = LIVE_TEXT_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// What an agent's turn reports about its tools and permissions, in the
/// worker's own terms. Worker-internal: the worker turns each into a
/// durable row write and a public `ToolActivity` event, so the public
/// stream never learns a new shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentActivityEvent {
    ToolStarted {
        invocation: ToolInvocationId,
        label: String,
        /// What the agent asked the tool to do, as the host relayed it
        /// (bounded there and here): a command line, a change's paths, an
        /// MCP call's arguments. Kept on the row.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
    },
    ToolCompleted {
        invocation: ToolInvocationId,
        tool: AgentToolKind,
        succeeded: bool,
        /// Why it did not succeed, as the host classified it — never the
        /// provider's own error text, which does not cross the session
        /// boundary. `None` when it succeeded, or when the host could not
        /// say.
        failure: Option<AgentToolFailure>,
        /// The tool's own account of the failure, as the host relayed it
        /// (bounded there): what the row shows under its outcome. `None`
        /// when it succeeded or the tool said nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// What the tool produced, as the host relayed it (bounded there
        /// and here): a command's output, a diff, an MCP result. Kept on
        /// the row.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    ApprovalRequired {
        invocation: ToolInvocationId,
        summary: String,
        choices: Vec<OfferedChoice>,
    },
    ApprovalResolved {
        invocation: ToolInvocationId,
        approved: bool,
    },
    ApprovalExpired {
        invocation: ToolInvocationId,
    },
}

/// Why a tool call did not succeed, in the host's closed vocabulary. The
/// chat keeps it as the row's error code; the row's words come from it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AgentToolFailure {
    /// Not allowed to run: an approval refused, a permission denied.
    Denied,
    /// Stopped before it finished.
    Cancelled,
    /// Ran out of time.
    TimedOut,
    /// Ran and reported an error.
    Errored,
    /// Did not succeed, and the host could not place why.
    Unclassified,
}

impl AgentToolFailure {
    /// The durable code the row keeps. Time and an unplaced failure both
    /// read as a failed execution: the row's word tells them apart only
    /// while the host's classification is in hand.
    pub fn error_code(self) -> ToolErrorCode {
        match self {
            Self::Denied => ToolErrorCode::PermissionDenied,
            Self::Cancelled => ToolErrorCode::Cancelled,
            Self::TimedOut | Self::Errored | Self::Unclassified => ToolErrorCode::ExecutionFailed,
        }
    }
}

/// A fact about the attempt the surface shows as one line and nothing
/// stores.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AttemptNotice {
    /// The agent could not be reattached and a replacement was started with
    /// the conversation so far.
    FreshStart,
    /// The agent takes no pictures: it was told where the files are instead
    /// (chat-attachments D4).
    ImagesAsFiles,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_tokens_round_trip_and_workspace_tools_are_not_agent_rows() {
        for kind in [
            AgentToolKind::Tool,
            AgentToolKind::Command,
            AgentToolKind::FileChange,
            AgentToolKind::Search,
            AgentToolKind::Mcp,
            AgentToolKind::Other,
            AgentToolKind::Approval,
        ] {
            assert_eq!(AgentToolKind::parse(kind.as_token()), Some(kind));
            assert!(AgentToolKind::is_agent_token(kind.as_token()));
        }
        assert!(!AgentToolKind::is_agent_token("workspace.read_document"));
    }

    #[test]
    fn live_text_is_cut_on_a_character_boundary() {
        let text = "é".repeat(LIVE_TEXT_LIMIT);
        let bounded = bound_live_text(&text);
        assert!(bounded.len() <= LIVE_TEXT_LIMIT);
        assert!(bounded.chars().all(|ch| ch == 'é'));
        assert_eq!(bound_live_text("short"), "short");
    }
}
