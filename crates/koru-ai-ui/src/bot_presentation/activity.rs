// SPDX-License-Identifier: MPL-2.0

//! Collapsed Bot activity uses observed purpose, never a command or result.
//! This is presentation only: no model request, authority or success inference.
use std::collections::HashMap;

use koru_ai_core::{AgentToolKind, CallShape, DocumentId, ToolActivity};
use koru_i18n::{Key, Lang, fill, t};
use serde_json::Value;

use crate::tool_groups::StepKind;

const MAX_LABEL: usize = 160;
const MAX_INPUT: usize = 16 * 1024;

/// Observed activity between calls; never inferred from a prompt or a timer.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) enum Progress {
    Waiting,
    #[default]
    Thinking,
    Typing,
    Searching,
}

impl Progress {
    pub(super) fn text(self, lang: Lang) -> &'static str {
        t(
            lang,
            match self {
                Self::Waiting => Key::AiActivityWaiting,
                Self::Thinking => Key::AiThinking,
                Self::Typing => Key::AiActivityTyping,
                Self::Searching => Key::AiStepSearchingWeb,
            },
        )
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Names<'a> {
    pub documents: Option<&'a HashMap<DocumentId, String>>,
    pub bots: Option<&'a HashMap<uuid::Uuid, String>>,
}

pub(crate) fn is_current(call: &ToolActivity) -> bool {
    !call.status.is_terminal()
        && !crate::tool_groups::is_card(call)
        && !crate::questions::is_question_row(call)
}

pub(super) fn line(call: &ToolActivity, kind: StepKind, lang: Lang, names: Names<'_>) -> String {
    let koru = koru_call(call)
        .map(|(tool, input)| {
            (
                tool,
                input.and_then(|input| serde_json::from_str::<Value>(input).ok()),
            )
        })
        .or_else(|| grok_call(call));
    if let Some((tool, args)) = koru
        && let Some(key) = operation(&tool)
    {
        if let Some((key, name)) = named_target(call, &tool, args.as_ref(), names) {
            return fill(t(lang, key), &[&name]);
        }
        return t(lang, key).into();
    }
    if let Some(label) = call.agent_label.as_deref().and_then(descriptive_label) {
        return label.into();
    }
    // Fetch and search share the provider's coarse Web category. Match only
    // exact tool names or its established Fetch title; never display the URL.
    let label = call
        .agent_label
        .as_deref()
        .unwrap_or(&call.tool_name)
        .trim();
    if matches!(label, "WebFetch" | "web_fetch" | "Fetch" | "browse_page")
        || (kind == StepKind::Web && label.starts_with("Fetch "))
    {
        return t(lang, Key::AiActivityReadWeb).into();
    }
    // Unlike the expanded group line, this must never append subject(), which
    // may be a command, URL, query, path or other private tool argument.
    let key = match kind {
        StepKind::Read => Key::AiStepReading,
        StepKind::SearchFiles => Key::AiStepSearchingFiles,
        StepKind::Web => Key::AiStepSearchingWeb,
        StepKind::Command => Key::AiStepRunning,
        StepKind::Edit => Key::AiStepEditing,
        StepKind::Koru => Key::AiStepUsingKoru,
        StepKind::Tool => match call.agent_label.as_deref().map(str::trim) {
            // Native ToolStarted does not carry a kind. These exact tool names
            // are useful before ToolCompleted supplies it, without reading args.
            Some(
                "Bash"
                | "Terminal"
                | "command_execution"
                | "exec_command"
                | "shell"
                | "run_terminal_command",
            ) => Key::AiStepRunning,
            Some("Edit" | "Write" | "file_change") => Key::AiStepEditing,
            Some("web_search" | "WebSearch" | "Web search" | "Web search:") => {
                Key::AiStepSearchingWeb
            }
            Some("read_file") => Key::AiStepReading,
            Some("list_dir" | "grep") => Key::AiStepSearchingFiles,
            _ => Key::AiStepUsingTool,
        },
        StepKind::Permission => Key::AiStepWaitingPermission,
    };
    t(lang, key).into()
}

fn operation(tool: &str) -> Option<Key> {
    Some(match tool {
        "search_tool" | "ToolSearch" => Key::AiActivityFindTools,
        "search_documents" => Key::AiActivitySearchPages,
        "list_documents" => Key::AiActivityListPages,
        "read_document" => Key::AiActivityReadPage,
        "create_document" => Key::AiActivityCreatePage,
        "edit_document" | "update_document" => Key::AiActivityEditPage,
        "list_routines" => Key::AiActivityListRoutines,
        "create_routine" => Key::AiActivityCreateRoutine,
        "edit_routine" => Key::AiActivityEditRoutine,
        "run_routine" => Key::AiActivityRunRoutine,
        "list_bots" => Key::AiActivityListBots,
        "message_bot" => Key::AiActivityAskBot,
        "read_older_messages" => Key::AiStepReading,
        _ => return None,
    })
}

/// The existing direct, Claude and Codex shapes. A shell string containing a
/// tool name is never parsed as a tool envelope. Inputs stay borrowed and bounded.
fn koru_call(call: &ToolActivity) -> Option<(String, Option<&str>)> {
    let input = call.agent_input.as_deref().filter(|s| s.len() <= MAX_INPUT);
    if !crate::agents::is_agent_row(call) {
        return Some((
            run_tool(&call.tool_name)
                .unwrap_or(&call.tool_name)
                .to_owned(),
            input,
        ));
    }
    let label = call.agent_label.as_deref().unwrap_or_default().trim();
    if label.len() > 256 {
        return None;
    }
    if let Some(tool) = run_tool(label) {
        return Some((tool.into(), input));
    }
    if let Some(CallShape::Koru(tool)) = CallShape::of(label) {
        return Some((tool, input));
    }
    if operation(label).is_some() {
        return Some((label.into(), input));
    }
    let mcp = call.tool_name == AgentToolKind::Mcp.as_token()
        || (matches!(
            AgentToolKind::parse(&call.tool_name),
            Some(AgentToolKind::Other | AgentToolKind::Tool)
        ) && matches!(label, "mcp_tool" | "mcpToolCall" | "MCP tool"));
    let (tool, args) = mcp.then_some(input).flatten().and_then(codex_call)?;
    Some((tool.into(), Some(args)))
}

fn codex_call(input: &str) -> Option<(&str, &str)> {
    let rest = input
        .strip_prefix("koru: ")
        .or_else(|| input.strip_prefix("koru_run: "))?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let tool = &rest[..end];
    (!tool.is_empty()
        && tool
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_'))
    .then_some((tool, rest[end..].trim()))
}

/// Grok's observed use_tool wrapper. Only a known Koru operation inside this
/// exact bounded envelope is interpreted; arbitrary nested JSON is not a tool.
fn grok_call(call: &ToolActivity) -> Option<(String, Option<Value>)> {
    if call.agent_label.as_deref() != Some("use_tool")
        || !matches!(
            AgentToolKind::parse(&call.tool_name),
            Some(AgentToolKind::Other | AgentToolKind::Tool | AgentToolKind::Mcp)
        )
    {
        return None;
    }
    let input = call
        .agent_input
        .as_deref()
        .filter(|input| input.len() <= MAX_INPUT)?;
    let mut wrapper: Value = serde_json::from_str(input).ok()?;
    let tool = run_tool(wrapper.get("tool_name")?.as_str()?)?.to_owned();
    let args = wrapper.get_mut("tool_input")?.take();
    args.is_object().then_some((tool, Some(args)))
}

/// The native Bot MCP server is named `koru_run`. Its Claude
/// namespace and observed flattened alias are accepted only for known actions;
/// a similar-looking unknown token never becomes descriptive UI text.
fn run_tool(label: &str) -> Option<&str> {
    label
        .strip_prefix("mcp__koru_run__")
        .or_else(|| label.strip_prefix("koru_run__"))
        .or_else(|| label.strip_prefix("koru_run_"))
        .filter(|tool| operation(tool).is_some())
}

fn named_target(
    call: &ToolActivity,
    tool: &str,
    args: Option<&Value>,
    names: Names<'_>,
) -> Option<(Key, String)> {
    let (key, target) = match tool {
        "read_document" | "edit_document" | "update_document" => {
            let document = call.target_document_id.or_else(|| {
                args?
                    .get("document_id")?
                    .as_str()?
                    .parse::<uuid::Uuid>()
                    .ok()
                    .map(DocumentId::from_uuid)
            });
            let title = call
                .display_target
                .as_deref()
                .or_else(|| names.documents?.get(&document?).map(String::as_str))?;
            (
                if tool == "read_document" {
                    Key::AiActivityReadNamed
                } else {
                    Key::AiActivityEditNamed
                },
                title,
            )
        }
        "create_document" => (Key::AiActivityCreateNamed, args?.get("title")?.as_str()?),
        "create_routine" => (Key::AiActivityCreateNamed, args?.get("name")?.as_str()?),
        "edit_routine" => (Key::AiActivityEditNamed, args?.get("name")?.as_str()?),
        "message_bot" => {
            let bot = args?.get("to")?.as_str()?.parse::<uuid::Uuid>().ok()?;
            (Key::AiActivityAskNamed, names.bots?.get(&bot)?.as_str())
        }
        _ => return None,
    };
    display_name(target).map(|name| (key, name))
}

/// Display-name fields are appropriate UI context, unlike arbitrary task/body
/// strings. Bound them on Unicode boundaries and refuse code, URLs, credential
/// syntax and opaque IDs rather than turning those into status text.
fn display_name(value: &str) -> Option<String> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if value.is_empty() || value.len() > 1024 || value.parse::<uuid::Uuid>().is_ok()
        || value.contains("://") || value.starts_with('/')
        || ["bearer ", "sk-", "api_key", "password=", "token="].iter().any(|part| lower.contains(part))
        || value.chars().any(|ch| ch.is_control() || matches!(ch,
            '`' | '$' | '{' | '}' | '\\' | '=' | '|' | ';' | '<' | '>'
                | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}'))
        || value.split_whitespace().any(|word| word.chars().count() > 40)
    {
        return None;
    }
    let mut chars = value.chars();
    let mut name: String = chars.by_ref().take(64).collect();
    if chars.next().is_some() {
        name.push('…');
    }
    Some(name)
}

/// A provider's short, already descriptive live label may be useful. Be
/// conservative: accept ordinary action prose, not generic tool tokens, shell
/// syntax, URLs, identifiers, credentials, multiline text or completion claims.
/// Unrecognized languages/phrases use the localized operation/kind fallback.
fn descriptive_label(label: &str) -> Option<&str> {
    let label = label.trim();
    if label.len() > MAX_LABEL
        || !label
            .chars()
            .all(|ch| ch.is_alphabetic() || matches!(ch, ' ' | '\'' | '’' | '…'))
    {
        return None;
    }
    let words: Vec<_> = label.split_whitespace().collect();
    if !(2..=16).contains(&words.len())
        || !matches!(
            *words.first()?,
            "Searching"
                | "Reading"
                | "Creating"
                | "Editing"
                | "Updating"
                | "Listing"
                | "Asking"
                | "Checking"
                | "Looking"
                | "Inspecting"
                | "Opening"
                | "Fetching"
                | "Reviewing"
                | "Comparing"
                | "Preparing"
                | "Running"
                | "Writing"
                | "Downloading"
                | "Connecting"
                | "Waiting"
                | "Unsubscribing"
        )
        || words.iter().any(|word| {
            word.len() > 24
                || matches!(
                    word.trim_end_matches('…').to_ascii_lowercase().as_str(),
                    "password"
                        | "secret"
                        | "token"
                        | "authorization"
                        | "bearer"
                        | "credential"
                        | "credentials"
                        | "cookie"
                        | "apikey"
                        | "completed"
                        | "finished"
                        | "succeeded"
                        | "successfully"
                        | "curl"
                        | "wget"
                        | "bash"
                        | "powershell"
                        | "pwsh"
                        | "cmd"
                        | "python"
                        | "node"
                        | "npm"
                        | "npx"
                        | "cargo"
                        | "rustc"
                )
        })
    {
        return None;
    }
    Some(label)
}

#[cfg(test)]
mod tests;
