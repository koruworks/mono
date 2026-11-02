// SPDX-License-Identifier: MPL-2.0

//! **MCP tools.** One of Koru's own reads as one friendly line — *Read a
//! document · Trip to Portugal · 42 blocks* — from the friendly-name table
//! the step lines use (`tool_groups::koru_tool_line`), with the result's
//! JSON one click away; any other MCP tool's result is pretty-printed
//! JSON, folded when long. A result that is not JSON is drawn as text.
//!
//! What agents send (D17, Step 0): Claude titles an MCP call by its name
//! (`mcp__koru__read_document`) and sends the tool's text content as its
//! output; Codex files it as `mcpToolCall`, with `server: tool {args}` as
//! its input and the call's `result` — `{"content": [{"type": "text",
//! …}], …}` — as its output. Every output is cut at the row's bound
//! (4,000 characters), so a large result — a whole document — is cut off,
//! no longer parses, and reads as text.

use koru_ai_core::CallShape;
use koru_i18n::{Key, Lang, fill, t};
use serde_json::Value;

use super::{Call, CodeLine, Ink, Piece, Rendered, code_block, unfence};
use crate::tool_groups::koru_tool_line;

/// The Koru tool a call named: by its label (`mcp__koru__read_document`),
/// or by Codex's input (`koru: read_document {…}`).
pub(crate) fn koru_tool(call: &Call<'_>) -> Option<String> {
    if let Some(CallShape::Koru(tool)) =
        call.activity.agent_label.as_deref().and_then(CallShape::of)
    {
        return Some(tool);
    }
    let rest = call.input?.strip_prefix("koru: ")?;
    let tool = rest.split_whitespace().next()?;
    tool.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        .then(|| tool.to_owned())
}

/// Whether a call is an MCP tool's: filed as one, or titled by an MCP
/// tool's name.
pub(crate) fn is_mcp(call: &Call<'_>) -> bool {
    call.activity.tool_name == koru_ai_core::AgentToolKind::Mcp.as_token()
        || call
            .activity
            .agent_label
            .as_deref()
            .is_some_and(|label| label.trim().starts_with("mcp__"))
}

pub(super) fn render_koru(call: &Call<'_>, lang: Lang, expanded: bool) -> Option<Rendered> {
    let tool = koru_tool(call)?;
    let friendly = koru_tool_line(&tool, lang);
    let value = call.output.and_then(result_json);
    let mut pieces = Vec::new();
    match &value {
        Some(value) => {
            let mut summary = vec![friendly];
            summary.extend(title_of(value));
            summary.extend(count_of(value, lang));
            pieces.push(Piece::Header {
                text: summary.join(" \u{00B7} "),
                mono: false,
            });
        }
        None => pieces.push(Piece::Header {
            text: friendly,
            mono: false,
        }),
    }
    super::error_block(call, expanded, lang, &mut pieces);
    match value {
        // The JSON behind the line is the person's to open.
        Some(value) => {
            let lines = pretty(&value);
            if expanded {
                pieces.push(Piece::Code(lines));
                pieces.push(Piece::Toggle(t(lang, Key::AiToolShowFewerLines).to_owned()));
            } else {
                pieces.push(Piece::Toggle(fill(
                    t(lang, Key::AiToolShowAllLines),
                    &[&lines.len().to_string()],
                )));
            }
        }
        None => {
            if let Some(output) = call.output {
                super::markdown(output, expanded, lang, &mut pieces);
            }
        }
    }
    Some(Rendered {
        pieces,
        notes: Vec::new(),
    })
}

pub(super) fn render_mcp(call: &Call<'_>, lang: Lang, expanded: bool) -> Option<Rendered> {
    let value = call.output.and_then(result_json)?;
    let mut pieces = Vec::new();
    if let Some(name) = mcp_name(call) {
        pieces.push(Piece::Header {
            text: name,
            mono: false,
        });
    }
    super::error_block(call, expanded, lang, &mut pieces);
    pieces.extend(code_block(pretty(&value), expanded, lang));
    Some(Rendered {
        pieces,
        notes: Vec::new(),
    })
}

/// `mcp__github__search_issues` → `github · search_issues`.
fn mcp_name(call: &Call<'_>) -> Option<String> {
    if let Some(label) = call.activity.agent_label.as_deref()
        && let Some(rest) = label.trim().strip_prefix("mcp__")
    {
        return Some(match rest.split_once("__") {
            Some((server, tool)) => format!("{server} \u{00B7} {tool}"),
            None => rest.to_owned(),
        });
    }
    // Codex: `server: tool {args}`.
    let input = call.input?;
    let (server, rest) = input.split_once(": ")?;
    let tool = rest.split_whitespace().next()?;
    Some(format!("{server} \u{00B7} {tool}"))
}

/// A result's JSON: the output itself, or — when it is an MCP result,
/// `{"content": [{"type": "text", "text": …}]}` — the JSON its text holds.
/// `None` when it is not JSON, or was cut off by the row's bound.
pub(crate) fn result_json(output: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(unfence(output).1.trim()).ok()?;
    if let Some(text) = value
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            let texts = blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text")?.as_str())
                .collect::<Vec<_>>();
            (!texts.is_empty()).then(|| texts.join("\n"))
        })
    {
        return serde_json::from_str::<Value>(text.trim())
            .ok()
            .filter(|inner| inner.is_object() || inner.is_array())
            .or(Some(Value::String(text)));
    }
    (value.is_object() || value.is_array()).then_some(value)
}

fn pretty(value: &Value) -> Vec<CodeLine> {
    let text = match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    text.lines()
        .map(|line| CodeLine::plain(line, Ink::Text))
        .collect()
}

/// What a result is about: its `title`, or a document's own title — the
/// text of its `document` node.
fn title_of(value: &Value) -> Option<String> {
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .or_else(|| {
            value.get("nodes")?.as_array()?.iter().find_map(|node| {
                (node.get("kind")?.as_str()? == "document").then(|| {
                    node.pointer("/content/text")
                        .and_then(Value::as_str)
                        .or_else(|| node.pointer("/metadata/name").and_then(Value::as_str))
                })?
            })
        })?
        .trim();
    (!title.is_empty()).then(|| title.chars().take(80).collect())
}

/// How much it holds: a document's blocks, or a list's results.
fn count_of(value: &Value, lang: Lang) -> Option<String> {
    if let Some(nodes) = value.get("nodes").and_then(Value::as_array) {
        let blocks = nodes
            .iter()
            .filter(|node| node.get("kind").and_then(Value::as_str) != Some("document"))
            .count();
        return Some(fill(t(lang, Key::AiToolBlockCount), &[&blocks.to_string()]));
    }
    let list = match value {
        Value::Array(items) => Some(items),
        Value::Object(map) => map.values().find_map(Value::as_array),
        _ => None,
    }?;
    Some(fill(
        t(lang, Key::AiToolResultCount),
        &[&list.len().to_string()],
    ))
}
