// SPDX-License-Identifier: MPL-2.0

use koru_session_protocol::{AgentUsage, ToolFailure, ToolKind};
use serde_json::Value;

/// One text model the app-server lists.
#[derive(Debug, Clone)]
pub(super) struct CodexModel {
    pub model: String,
    pub image: bool,
    pub is_default: bool,
    /// What the app-server calls it for a person (`displayName`), and its
    /// `description`, within `ModelLabel`'s bounds.
    pub display_name: Option<koru_session_protocol::ModelLabel>,
    pub description: Option<koru_session_protocol::ModelLabel>,
    /// Its `supportedReasoningEfforts` and `defaultReasoningEffort`, in
    /// Koru's vocabulary: the catalog's levels, and the ones a chosen effort
    /// is checked against.
    pub reasoning: Option<koru_session_protocol::ModelReasoning>,
    /// Codex names a newer model to move to (`upgrade` is set): its own mark
    /// for an older one (D11).
    pub legacy: bool,
    /// The model's *Fast* service tier (koru-ai D11), when it lists one.
    pub fast: Option<FastTier>,
}

/// **Codex's Fast mode**: the service tier its catalog names *Fast* (seen as
/// `priority` on 0.166.0), and whether the model runs in it by default
/// (`defaultServiceTier`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FastTier {
    pub id: String,
    pub on_by_default: bool,
}

pub(super) struct ModelPage {
    /// Every text model on the page, in the provider's order. A model that
    /// takes no text is not one a chat can use and is left out; a default
    /// that takes no text is a malformed page.
    pub models: Vec<CodexModel>,
    pub next_cursor: Option<String>,
}

pub(super) fn model_page(response: &Value) -> Option<ModelPage> {
    let data = response.get("data")?.as_array()?;
    let mut models = Vec::new();
    for model in data {
        let is_default = model.get("isDefault").and_then(Value::as_bool) == Some(true);
        match text_model(model, is_default) {
            Some(model) => models.push(model),
            None if is_default => return None,
            None => {}
        }
    }
    let next_cursor = match response.get("nextCursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(cursor)) if valid_token(cursor, 512) => Some(cursor.clone()),
        _ => return None,
    };
    Some(ModelPage {
        models,
        next_cursor,
    })
}

fn text_model(model: &Value, is_default: bool) -> Option<CodexModel> {
    let modalities = model.get("inputModalities")?.as_array()?;
    if !modalities
        .iter()
        .any(|value| value.as_str() == Some("text"))
    {
        return None;
    }
    let label = |key: &str| model.get(key).and_then(Value::as_str);
    let display_name = label("displayName").and_then(koru_session_protocol::ModelLabel::name);
    let description = label("description").and_then(koru_session_protocol::ModelLabel::description);
    let efforts = model
        .get("supportedReasoningEfforts")
        .and_then(Value::as_array)
        .map(|efforts| {
            efforts
                .iter()
                .filter_map(|effort| effort.get("reasoningEffort").and_then(Value::as_str))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let (reasoning, _) = koru_session_protocol::ModelReasoning::from_agent_tokens(
        efforts,
        label("defaultReasoningEffort"),
    );
    let legacy = model
        .get("upgrade")
        .is_some_and(|upgrade| !upgrade.is_null());
    let fast = model
        .get("serviceTiers")
        .and_then(Value::as_array)
        .and_then(|tiers| {
            tiers.iter().find(|tier| {
                tier.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.trim().eq_ignore_ascii_case("fast"))
            })
        })
        .and_then(|tier| tier.get("id")?.as_str())
        .filter(|id| valid_token(id, 64))
        .map(|id| FastTier {
            id: id.to_owned(),
            on_by_default: label("defaultServiceTier") == Some(id),
        });
    let model = model.get("model")?.as_str()?;
    if !valid_token(model, 128) {
        return None;
    }
    Some(CodexModel {
        reasoning,
        legacy,
        fast,
        model: model.to_owned(),
        image: modalities
            .iter()
            .any(|value| value.as_str() == Some("image")),
        is_default,
        display_name,
        description,
    })
}

fn valid_token(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= maximum_bytes && !value.chars().any(char::is_control)
}

/// Codex's item type, as one of the kinds Koru names. `None` is an item
/// that is not a tool call at all.
///
/// The provider's own string never leaves this function: what the adapter
/// emits is the token, and the label the session view shows is
/// [`ToolKind::as_str`] — the same six words it has always shown.
pub(super) fn tool_kind(item_type: &str) -> Option<ToolKind> {
    match item_type {
        "commandExecution" => Some(ToolKind::CommandExecution),
        "fileChange" => Some(ToolKind::FileChange),
        "mcpToolCall" => Some(ToolKind::McpTool),
        "dynamicToolCall" => Some(ToolKind::DynamicTool),
        "webSearch" => Some(ToolKind::WebSearch),
        "imageGeneration" => Some(ToolKind::ImageGeneration),
        _ => None,
    }
}

pub(super) fn item_succeeded(item: &Value) -> bool {
    item.get("status").and_then(Value::as_str) == Some("completed")
        && item.get("error").is_none_or(Value::is_null)
}

/// The item statuses this adapter can place, and the class each one means.
/// Compared case-insensitively, because a status is a wire keyword and its
/// spelling is the provider's business.
///
/// **Everything not on this list is [`ToolFailure::Unclassified`].** The
/// list grows when a version's compatibility note shows a new status, never
/// by pattern-matching on the shape of a word.
const FAILURE_CLASSES: [(&str, ToolFailure); 11] = [
    ("cancelled", ToolFailure::Cancelled),
    ("canceled", ToolFailure::Cancelled),
    ("aborted", ToolFailure::Cancelled),
    ("interrupted", ToolFailure::Cancelled),
    ("rejected", ToolFailure::Denied),
    ("declined", ToolFailure::Denied),
    ("denied", ToolFailure::Denied),
    ("timedout", ToolFailure::TimedOut),
    ("timeout", ToolFailure::TimedOut),
    ("failed", ToolFailure::Errored),
    ("error", ToolFailure::Errored),
];

/// **Why the call did not work, as a class — never as the provider's
/// words.**
///
/// `None` for a call that succeeded. The provider's `error` value is read
/// only for *whether it is there*: not its message, not its code, not one
/// byte of it, because it is prose that quotes the command that ran and
/// what came back.
pub(super) fn item_failure(item: &Value) -> Option<ToolFailure> {
    if item_succeeded(item) {
        return None;
    }
    let status = item
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some((_, class)) = FAILURE_CLASSES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(status))
    {
        return Some(*class);
    }
    // A status the adapter cannot place, with the provider having named an
    // error beside it: it errored, which is all that is known.
    if item.get("error").is_some_and(|error| !error.is_null()) {
        return Some(ToolFailure::Errored);
    }
    Some(ToolFailure::Unclassified)
}

/// The failed item's own message, when the app-server put one beside it:
/// `error` as a string, or `error.message`. Bounded by the protocol
/// (v17); nothing else of the item is read for its words.
pub(super) fn item_failure_detail(item: &Value) -> Option<String> {
    let error = item.get("error")?;
    error
        .as_str()
        .or_else(|| error.get("message").and_then(Value::as_str))
        .and_then(koru_session_protocol::bound_tool_detail)
}

/// What the item was asked to do (UC17), by its kind: a command as its
/// line, a file change as its paths, an MCP call as `server: tool` and its
/// arguments. Bounded by the protocol; a kind this adapter does not read
/// gives nothing.
pub(super) fn item_input(item: &Value) -> Option<String> {
    let text = match item.get("type").and_then(Value::as_str)? {
        "commandExecution" => match item.get("command")? {
            Value::String(command) => command.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
            _ => return None,
        },
        "fileChange" => item
            .get("changes")?
            .as_array()?
            .iter()
            .filter_map(|change| change.get("path").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        "mcpToolCall" => {
            let server = item
                .get("server")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or_default();
            let arguments = item
                .get("arguments")
                .filter(|value| !value.is_null())
                .and_then(|value| serde_json::to_string(value).ok())
                .unwrap_or_default();
            format!("{server}: {tool} {arguments}")
        }
        // The query a web search ran, so its row says what it looked for.
        "webSearch" => item.get("query")?.as_str()?.to_owned(),
        _ => return None,
    };
    koru_session_protocol::bound_tool_content(&text)
}

/// What the item produced (UC17), by its kind: a command's aggregated
/// output, a file change's diffs, an MCP call's result. Bounded by the
/// protocol.
pub(super) fn item_output(item: &Value) -> Option<String> {
    let text = match item.get("type").and_then(Value::as_str)? {
        // A command that exited non-zero says so first, as Claude's do:
        // the chat reads `Exit code N` off the output's first line.
        "commandExecution" => {
            let output = item.get("aggregatedOutput").and_then(Value::as_str);
            match item
                .get("exitCode")
                .and_then(Value::as_i64)
                .filter(|code| *code != 0)
            {
                Some(code) => format!("Exit code {code}\n{}", output.unwrap_or_default()),
                None => output?.to_owned(),
            }
        }
        // Each file's diff under its path, so the chat can tell the files
        // of one change apart (D17).
        "fileChange" => item
            .get("changes")?
            .as_array()?
            .iter()
            .filter_map(|change| {
                let diff = change.get("diff").and_then(Value::as_str)?;
                Some(match change.get("path").and_then(Value::as_str) {
                    Some(path) if !diff.starts_with("--- ") && !diff.starts_with("diff --git") => {
                        format!("--- {path}\n+++ {path}\n{diff}")
                    }
                    _ => diff.to_owned(),
                })
            })
            .collect::<Vec<_>>()
            .join("\n"),
        "mcpToolCall" => match item.get("result")? {
            Value::Null => return None,
            Value::String(result) => result.clone(),
            other => serde_json::to_string(other).ok()?,
        },
        _ => return None,
    };
    koru_session_protocol::bound_tool_content(&text)
}

/// A thread's cumulative token counts as the app-server reports them under
/// `/tokenUsage/total`. A field the provider left out is `None` here and
/// stays `None` in every difference taken from it: unavailable, never zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct UsageTotals {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cached_input: Option<u64>,
}

impl UsageTotals {
    /// A fresh ephemeral thread has used nothing.
    pub const ZERO: Self = Self {
        input: Some(0),
        output: Some(0),
        cached_input: Some(0),
    };
}

pub(super) fn usage_totals(params: &Value) -> Option<UsageTotals> {
    let total = params.pointer("/tokenUsage/total")?;
    Some(UsageTotals {
        input: integer(total.get("inputTokens")),
        output: integer(total.get("outputTokens")),
        cached_input: integer(total.get("cachedInputTokens")),
    })
}

/// One turn's usage: the thread total now, less the thread total when the
/// turn was submitted. Codex's `inputTokens` already counts its cached
/// tokens (its own accounting subtracts `cachedInputTokens` from it to get
/// the uncached input), which is what `Included` declares.
pub(super) fn turn_usage(
    total: UsageTotals,
    baseline: UsageTotals,
    model: Option<koru_session_protocol::ModelName>,
) -> AgentUsage {
    AgentUsage {
        input_tokens: difference(total.input, baseline.input),
        output_tokens: difference(total.output, baseline.output),
        cached_input_tokens: difference(total.cached_input, baseline.cached_input),
        cached_input: koru_session_protocol::CachedInputRelationship::Included,
        model,
    }
}

/// A model name the provider reported, bounded by `ModelName`'s own rule;
/// a value that is not a name is nothing rather than a guess.
pub(super) fn model_name(value: Option<&Value>) -> Option<koru_session_protocol::ModelName> {
    value
        .and_then(Value::as_str)
        .and_then(|name| name.parse().ok())
}

fn difference(now: Option<u64>, baseline: Option<u64>) -> Option<u64> {
    Some(now?.saturating_sub(baseline?))
}

fn integer(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64)
}
