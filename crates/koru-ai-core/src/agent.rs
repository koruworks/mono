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
    /// Reading a file. No agent protocol names it yet: a call is filed here
    /// when its label has the shape of a read ([`CallShape::Read`]).
    Read,
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
            Self::Read => "agent.read",
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
            "agent.read" => Self::Read,
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

    /// The kind a completed call is filed under: what the agent said, or —
    /// when it said only *a tool* — what the call's label has the shape of.
    pub fn refined(self, label: Option<&str>) -> Self {
        if !matches!(self, Self::Tool | Self::Other) {
            return self;
        }
        match label.and_then(CallShape::of) {
            Some(CallShape::Read) => Self::Read,
            Some(CallShape::Search | CallShape::Fetch) => Self::Search,
            _ => self,
        }
    }
}

/// What a call's label says it did, read from the label's shape: an agent
/// whose protocol files every call but commands and edits as *other* still
/// titles a read *Read README.md* and a lookup *Find `*.rs`*.
///
/// This is the one place a label's wording is matched. It decides only how
/// a call is summarized; nothing is refused or routed by it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallShape {
    /// Reading a file: *Read src/main.rs*.
    Read,
    /// Looking through local files: *Find …*, *grep …*, *List …*.
    Search,
    /// Reaching the web: *Fetch https://…*, or a quoted query.
    Fetch,
    /// Looking up which tools exist — plumbing a person has no use for.
    ToolDiscovery,
    /// One of Koru's own tools, reached through its MCP server; the tool's
    /// own name (`read_document`).
    Koru(String),
}

impl CallShape {
    pub fn of(label: &str) -> Option<Self> {
        let label = label.trim();
        if let Some(tool) = koru_tool(label) {
            return Some(Self::Koru(tool));
        }
        let first = label.split_whitespace().next()?;
        let rest = label[first.len()..].trim_start();
        Some(match first {
            "ToolSearch" => Self::ToolDiscovery,
            // Claude titles a web search *Search "the query"*: a quoted
            // query is the web's, a pattern the files'.
            "Search" if rest.len() > 2 && rest.starts_with('"') && rest.ends_with('"') => {
                Self::Fetch
            }
            // A picture an agent looked at is a file it read: Codex's
            // image viewer is labelled with its tool's own name.
            "Read" | "view_image" => Self::Read,
            "Find" | "Glob" | "grep" | "Grep" | "List" | "Search" => Self::Search,
            "Fetch" => Self::Fetch,
            _ if label.len() > 2 && label.starts_with('"') && label.ends_with('"') => Self::Fetch,
            _ => return None,
        })
    }
}

/// `koru`'s tool in a label that names an MCP server and one of its tools:
/// `mcp__koru__read_document`, `koru.read_document`, `koru/read_document`.
fn koru_tool(label: &str) -> Option<String> {
    let rest = label.strip_prefix("mcp__").unwrap_or(label);
    let lower = rest.to_ascii_lowercase();
    let tool = ["koru__", "koru.", "koru/", "koru:"]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))?;
    let tool = tool.split_whitespace().next()?;
    (!tool.is_empty()
        && tool
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_'))
    .then(|| tool.to_owned())
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
    /// The questions a waiting question set asks (koru-ai D108), in the
    /// agent's order with each one's options in its order; empty once
    /// answered, skipped or expired, and on every other row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<crate::AgentQuestion>,
}

/// The longest label or summary that crosses to the surface. Anything
/// longer is cut at a character boundary; the agent's wording is a courtesy,
/// not a record.
pub const LIVE_TEXT_LIMIT: usize = 512;

/// The most of a tool call's input or output the chat keeps, in characters
/// — the same bound the session protocol applies before it crosses, applied
/// again here so a row never carries more than its column admits. A JSON
/// result too long for it already arrives as a summary that fits, marked
/// `_truncated` (`koru-session-runtime`'s `providers::tool_output`), so this
/// cut leaves it whole.
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
    /// The agent moved its approval mode itself during the reply (koru-ai
    /// D7): Claude leaving plan mode once its plan is approved, or landing
    /// on another mode than the one asked. `started_in` is the mode the
    /// reply was sent in, so a choice the person made meanwhile is kept.
    ModeChanged {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_in: Option<String>,
        mode: String,
    },
    /// A short status line from the agent that is not part of its reply
    /// (koru-ai D69): kept with the reply, shown beside it.
    Notice {
        text: String,
    },
    /// The agent stopped to ask the person (koru-ai D108): a row of its
    /// own, filed as a permission request. `blocking: false` is a set the
    /// agent does not wait on — its reply runs on, and the row waits
    /// without stopping it. Closed by [`Self::QuestionsAnswered`], or by
    /// [`Self::ApprovalExpired`] when the turn ends first.
    QuestionsAsked {
        invocation: ToolInvocationId,
        questions: Vec<crate::AgentQuestion>,
        blocking: bool,
        /// A set the agent asked in its reply and moved on from: it stays
        /// answerable after the reply ends, its answers go as the person's
        /// next message, and another message passes it over.
        #[serde(default)]
        next_message: bool,
    },
    /// The set was answered — a secret's answer withheld — or, with
    /// `None`, skipped.
    QuestionsAnswered {
        invocation: ToolInvocationId,
        answers: Option<Vec<crate::AgentAnswer>>,
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
            AgentToolKind::Read,
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
    fn a_labels_shape_says_what_a_call_did() {
        assert_eq!(CallShape::of("Read README.md"), Some(CallShape::Read));
        assert_eq!(CallShape::of("view_image"), Some(CallShape::Read));
        assert_eq!(CallShape::of("Find `**/*.rs`"), Some(CallShape::Search));
        assert_eq!(CallShape::of("grep -n \"todo\""), Some(CallShape::Search));
        assert_eq!(
            CallShape::of("Fetch https://example.com"),
            Some(CallShape::Fetch)
        );
        assert_eq!(CallShape::of("\"koru notes app\""), Some(CallShape::Fetch));
        assert_eq!(
            CallShape::of("Search \"koru notes app\""),
            Some(CallShape::Fetch)
        );
        assert_eq!(
            CallShape::of("Search src for todo"),
            Some(CallShape::Search)
        );
        assert_eq!(CallShape::of("ToolSearch"), Some(CallShape::ToolDiscovery));
        assert_eq!(
            CallShape::of("mcp__koru__read_document"),
            Some(CallShape::Koru("read_document".into()))
        );
        assert_eq!(
            CallShape::of("koru.search_documents"),
            Some(CallShape::Koru("search_documents".into()))
        );
        assert_eq!(CallShape::of("`git status`"), None);
        assert_eq!(CallShape::of("Reading"), None);
        assert_eq!(CallShape::of("\""), None);
        assert_eq!(CallShape::of("mcp__korus__x"), None);
    }

    #[test]
    fn only_an_unnamed_kind_is_refined_by_its_label() {
        assert_eq!(
            AgentToolKind::Other.refined(Some("Read a.txt")),
            AgentToolKind::Read
        );
        assert_eq!(
            AgentToolKind::Tool.refined(Some("Find x")),
            AgentToolKind::Search
        );
        assert_eq!(
            AgentToolKind::Command.refined(Some("Read a.txt")),
            AgentToolKind::Command
        );
        assert_eq!(AgentToolKind::Other.refined(None), AgentToolKind::Other);
        assert_eq!(
            AgentToolKind::Other.refined(Some("mcp__koru__read_document")),
            AgentToolKind::Other
        );
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
