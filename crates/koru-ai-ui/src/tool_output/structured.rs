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
//! (4,000 characters). A JSON result too long for it is kept as a valid
//! **summary** carrying a `_truncated` marker — the result's size, each
//! list's full length, a document's block count — so the line still counts
//! the whole result and says it is a summary; any other output too long for
//! the bound is cut off, no longer parses, and reads as text.

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
        // A summary kept in place of a result too long for the row: how
        // large the result was is said instead of *Show all*, because the
        // whole result is not there to open.
        Some(value) if summarized_bytes(&value).is_some() => {
            let bytes = summarized_bytes(&value).unwrap_or_default();
            pieces.push(Piece::Meta(fill(
                t(lang, Key::AiToolResultSummarized),
                &[&bytes.div_ceil(1024).max(1).to_string()],
            )));
        }
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
    // A summary's marker is bookkeeping, not the tool's answer.
    pieces.extend(code_block(pretty(&without_marker(&value)), expanded, lang));
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

/// The marker a result too long for the row carries once it was kept as a
/// summary (`koru-session-runtime`'s `providers::tool_output`):
/// `{"bytes": N, "lengths": {key: total}, "counts": {"blocks": B, …}}`.
const TRUNCATED_KEY: &str = "_truncated";

/// The size of the result a summary stands for; `None` for a whole result.
fn summarized_bytes(value: &Value) -> Option<u64> {
    value.get(TRUNCATED_KEY)?.get("bytes")?.as_u64()
}

/// The value without a summary's marker, as it is shown.
fn without_marker(value: &Value) -> Value {
    let mut value = value.clone();
    if let Value::Object(map) = &mut value {
        map.remove(TRUNCATED_KEY);
    }
    value
}

/// How much it holds: a document's blocks, or a list's results. A summary
/// keeps only a list's first items, so its marker's totals are the count.
fn count_of(value: &Value, lang: Lang) -> Option<String> {
    let marker = value.get(TRUNCATED_KEY);
    if let Some(nodes) = value.get("nodes").and_then(Value::as_array) {
        let blocks = marker
            .and_then(|marker| marker.pointer("/counts/blocks"))
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                nodes
                    .iter()
                    .filter(|node| node.get("kind").and_then(Value::as_str) != Some("document"))
                    .count() as u64
            });
        return Some(fill(t(lang, Key::AiToolBlockCount), &[&blocks.to_string()]));
    }
    let (key, list) = match value {
        Value::Array(items) => Some((None, items)),
        Value::Object(map) => map
            .iter()
            .find_map(|(key, value)| value.as_array().map(|items| (Some(key), items))),
        _ => None,
    }?;
    let total = key
        .and_then(|key| marker?.get("lengths")?.get(key)?.as_u64())
        .unwrap_or(list.len() as u64);
    Some(fill(t(lang, Key::AiToolResultCount), &[&total.to_string()]))
}
