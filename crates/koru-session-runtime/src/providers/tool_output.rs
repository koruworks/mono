// SPDX-License-Identifier: MPL-2.0

//! **What a tool produced, within the protocol's bound — and still JSON when
//! it was JSON.**
//!
//! Every adapter bounds a tool's output to [`TOOL_CONTENT_CHARS`] before it
//! crosses. A plain cut is right for a command's output or a diff, whose
//! first screen is what a person reads. It is wrong for a JSON result: a
//! whole document read through Koru's `read_document` is tens of kilobytes,
//! and its first 4,000 characters are not JSON at all, so the work view can
//! no longer say *Read a document · Trip to Portugal · 42 blocks* and shows
//! a wall of half an object instead.
//!
//! So a JSON result that does not fit is replaced by a **summary** that does:
//! its scalar fields (a document's id, its revision), the first few items of
//! each list, each list's full length, and a marker that says it is one —
//!
//! ```json
//! {"document_id": "…", "revision": "…", "nodes": [{…the document node…}, …],
//!  "_truncated": {"bytes": 51234, "lengths": {"nodes": 213},
//!                 "counts": {"blocks": 212, "tasks": 12}}}
//! ```
//!
//! `bytes` is the size of the result that was summarized; `lengths` is every
//! top-level list's full length (the summary keeps only its first items);
//! `counts` is present for a document's `nodes` — its blocks (every node but
//! the document's own) and its tasks. A list's document node is always kept,
//! because its text is the document's title.
//!
//! An MCP result envelope (`{"content": [{"type": "text", "text": …}], …}`,
//! which is what Codex relays) is unwrapped first: the JSON the tool returned
//! is what is summarized, and is kept whole when it alone fits. Output that
//! is not JSON keeps the plain cut, on a character boundary.

use koru_session_protocol::{TOOL_CONTENT_CHARS, bound_tool_content};
use serde_json::{Map, Value};

/// The marker a summary carries, so a reader can tell a summary from the
/// result itself.
pub(crate) const TRUNCATED_KEY: &str = "_truncated";

/// `text`, trimmed and bounded to [`TOOL_CONTENT_CHARS`] characters; a JSON
/// result too long for the bound becomes its summary. `None` when nothing
/// readable is left.
pub(crate) fn bound_tool_output(text: &str) -> Option<String> {
    let text = text.trim();
    if fits(text) {
        return bound_tool_content(text);
    }
    summarize(text).or_else(|| bound_tool_content(text))
}

/// How much each pass of the summary keeps. Each is smaller than the last;
/// the first whose summary fits is the one sent.
#[derive(Clone, Copy)]
struct Budget {
    /// Items kept from each list.
    items: usize,
    /// Characters kept of each string.
    chars: usize,
    /// Fields kept of each object.
    keys: usize,
}

const BUDGETS: [Budget; 4] = [
    Budget {
        items: 5,
        chars: 120,
        keys: 24,
    },
    Budget {
        items: 3,
        chars: 80,
        keys: 16,
    },
    Budget {
        items: 1,
        chars: 48,
        keys: 8,
    },
    Budget {
        items: 0,
        chars: 32,
        keys: 6,
    },
];

fn fits(text: &str) -> bool {
    // A byte is at most one character, so a short string is counted free.
    text.len() <= TOOL_CONTENT_CHARS || text.chars().count() <= TOOL_CONTENT_CHARS
}

/// The summary of a JSON result too long for the bound; `None` when `text`
/// is not a JSON object or array.
fn summarize(text: &str) -> Option<String> {
    let body = unfenced(text);
    let value: Value = serde_json::from_str(body).ok()?;
    let (value, bytes) = match mcp_text(&value) {
        Some(inner) => {
            let inner = inner.trim();
            match serde_json::from_str::<Value>(inner) {
                Ok(value @ (Value::Object(_) | Value::Array(_))) => {
                    // The envelope repeats the result (as text and as
                    // structured content); the result alone may fit.
                    if fits(inner) {
                        return Some(inner.to_owned());
                    }
                    (value, inner.len())
                }
                // A tool that answered in prose: the prose, cut.
                _ => return bound_tool_content(inner),
            }
        }
        None => (value, body.len()),
    };
    if !(value.is_object() || value.is_array()) {
        return None;
    }
    BUDGETS
        .iter()
        .filter_map(|budget| serde_json::to_string(&summary(&value, bytes, *budget)).ok())
        .find(|summary| fits(summary))
        .or_else(|| {
            let mut marker = Map::new();
            marker.insert("bytes".into(), bytes.into());
            let mut only = Map::new();
            only.insert(TRUNCATED_KEY.into(), Value::Object(marker));
            serde_json::to_string(&Value::Object(only)).ok()
        })
}

/// A result wrapped in a code fence, unwrapped.
fn unfenced(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let Some((_, body)) = rest.split_once('\n') else {
        return text;
    };
    body.trim_end().strip_suffix("```").unwrap_or(body).trim()
}

/// An MCP result envelope's text content, joined; `None` when `value` is
/// not one.
fn mcp_text(value: &Value) -> Option<String> {
    let texts = value
        .get("content")?
        .as_array()?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text")?.as_str())
        .collect::<Vec<_>>();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

fn summary(value: &Value, bytes: usize, budget: Budget) -> Value {
    let fields: Vec<(&str, &Value)> = match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, field)| (key.as_str(), field))
            .collect(),
        // A bare list is named so the marker can count it.
        other => vec![("items", other)],
    };
    let mut out = Map::new();
    let mut lengths = Map::new();
    let mut counts = None;
    for (key, field) in fields {
        if out.len() >= budget.keys {
            break;
        }
        match field {
            Value::Array(items) => {
                lengths.insert(key.to_owned(), items.len().into());
                if key == "nodes" {
                    counts = counts.or_else(|| node_counts(items));
                }
                out.insert(key.to_owned(), Value::Array(first_items(items, budget)));
            }
            field => {
                if let Some(field) = compact(field, 1, budget) {
                    out.insert(key.to_owned(), field);
                }
            }
        }
    }
    let mut marker = Map::new();
    marker.insert("bytes".into(), bytes.into());
    if !lengths.is_empty() {
        marker.insert("lengths".into(), Value::Object(lengths));
    }
    if let Some(counts) = counts {
        marker.insert("counts".into(), counts);
    }
    out.insert(TRUNCATED_KEY.into(), Value::Object(marker));
    Value::Object(out)
}

/// The first items of a top-level list, compacted — and a document's own
/// node wherever it is, because its text is the document's title.
fn first_items(items: &[Value], budget: Budget) -> Vec<Value> {
    let document = items.iter().position(|item| kind(item) == Some("document"));
    let mut picked: Vec<&Value> = document.map(|at| &items[at]).into_iter().collect();
    picked.extend(
        items
            .iter()
            .enumerate()
            .filter(|(at, _)| Some(*at) != document)
            .map(|(_, item)| item)
            .take(budget.items.saturating_sub(picked.len())),
    );
    picked
        .into_iter()
        .filter_map(|item| compact(item, 1, budget))
        .collect()
}

/// A document's blocks (every node but its own) and tasks; `None` when the
/// list's items carry no `kind`.
fn node_counts(nodes: &[Value]) -> Option<Value> {
    let kinds: Vec<&str> = nodes.iter().filter_map(kind).collect();
    if kinds.is_empty() {
        return None;
    }
    let mut counts = Map::new();
    let blocks = kinds.iter().filter(|kind| **kind != "document").count();
    let tasks = kinds.iter().filter(|kind| **kind == "task").count();
    counts.insert("blocks".into(), blocks.into());
    counts.insert("tasks".into(), tasks.into());
    Some(Value::Object(counts))
}

fn kind(item: &Value) -> Option<&str> {
    item.get("kind")?.as_str()
}

/// `value`, small: strings cut, nulls and empty containers dropped, and a
/// list or an object kept only up to two levels below the top — a node's
/// `content.text` is kept, its runs and attributes are not.
fn compact(value: &Value, depth: usize, budget: Budget) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::Bool(_) | Value::Number(_) => Some(value.clone()),
        Value::String(text) => Some(Value::String(cut(text, budget.chars))),
        Value::Array(items) => {
            if depth > 2 {
                return None;
            }
            let kept: Vec<Value> = items
                .iter()
                .take(budget.items.min(3))
                .filter_map(|item| compact(item, depth + 1, budget))
                .collect();
            (!kept.is_empty()).then_some(Value::Array(kept))
        }
        Value::Object(map) => {
            if depth > 2 {
                return None;
            }
            let kept: Map<String, Value> = map
                .iter()
                .filter_map(|(key, field)| Some((key.clone(), compact(field, depth + 1, budget)?)))
                .take(budget.keys)
                .collect();
            (!kept.is_empty()).then_some(Value::Object(kept))
        }
    }
}

/// `text` cut to `chars` characters, with an ellipsis when it was.
fn cut(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((end, _)) => format!("{}\u{2026}", &text[..end]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests;
