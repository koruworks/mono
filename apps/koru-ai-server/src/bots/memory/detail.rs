// SPDX-License-Identifier: MPL-2.0
use super::*;
use history::VerifiedMemory;
use koru_core::schema::{NodeContentRead, NodeContentReadError};
use koru_core::{Content, Parent, TextRun, Utf8Pos};
use serde_json::{Value, json};

/// Internal traversal only; an authenticated cursor is a later adapter's job.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::bots) struct MemoryDetailPosition {
    pub node_ordinal: usize,
    pub start_utf8: usize,
}

#[derive(Clone, Copy)]
pub(in crate::bots) struct DetailBudget {
    pub max_page_bytes: usize,
    pub continuation_reserve_bytes: usize,
}
impl Default for DetailBudget {
    fn default() -> Self {
        Self {
            max_page_bytes: 4096,
            continuation_reserve_bytes: 2048,
        }
    }
}

#[derive(Debug)]
pub(in crate::bots) struct MemoryDetailPage {
    pub chunks: Vec<Value>,
    pub complete: bool,
    pub next: Option<MemoryDetailPosition>,
}

fn fits(chunks: &[Value], complete: bool, budget: DetailBudget) -> bool {
    // A later cursor uses unescaped base64url, no padding. This includes the
    // entire envelope's framing; next-position fields are not wire fields.
    let continuation = (!complete).then(|| "a".repeat(budget.continuation_reserve_bytes));
    serde_json::to_vec(&json!({"chunks":chunks,"complete":complete,"continuation":continuation}))
        .is_ok_and(|bytes| bytes.len() <= budget.max_page_bytes)
}

fn strict_error(error: NodeContentReadError) -> MemoryCodecError {
    match error {
        NodeContentReadError::TooLarge => MemoryCodecError::DetailTooLarge,
        NodeContentReadError::UnsupportedDetail => MemoryCodecError::UnsupportedDetail,
        NodeContentReadError::InvalidContent => MemoryCodecError::InvalidSnapshot,
    }
}

fn walk(memory: &VerifiedMemory, fact: NodeId) -> Result<Vec<NodeId>> {
    if !memory.source.facts.iter().any(|f| f.node == fact) {
        return Err(MemoryCodecError::InvalidDetailPosition);
    }
    let mut stack = vec![(fact, 0)];
    let mut visited = std::collections::HashSet::new();
    let mut nodes = Vec::new();
    while let Some((node, depth)) = stack.pop() {
        if depth > DEPTH_LIMIT || !visited.insert(node) || visited.len() > NODE_LIMIT {
            return Err(MemoryCodecError::InvalidSnapshot);
        }
        nodes.push(node);
        stack.extend(
            memory
                .doc
                .children(Parent::Node(node))
                .map_err(|_| MemoryCodecError::InvalidSnapshot)?
                .into_iter()
                .rev()
                .map(|child| (child, depth + 1)),
        );
    }
    Ok(nodes)
}

fn chunk(
    memory: &VerifiedMemory,
    node: NodeId,
    value: &NodeContentRead,
    start: usize,
    end: usize,
) -> Result<Value> {
    let (content, runs, range) = match &value.content {
        Content::Text { text, attrs } => {
            let runs: Vec<TextRun> = value
                .runs
                .iter()
                .filter_map(|run| {
                    let a = run.start.0.max(start);
                    let b = run.end.0.min(end);
                    if a >= b {
                        return None;
                    }
                    Some(TextRun {
                        start: Utf8Pos(a - start),
                        end: Utf8Pos(b - start),
                        text: text[a..b].to_owned(),
                        bold: run.bold,
                        italic: run.italic,
                        underline: run.underline,
                        strike: run.strike,
                        code: run.code,
                        style: run.style.clone(),
                        pill: run.pill,
                    })
                })
                .collect();
            (
                Content::Text {
                    text: text[start..end].to_owned(),
                    attrs: attrs.clone(),
                },
                runs,
                (Some(start), Some(end)),
            )
        }
        other => (other.clone(), Vec::new(), (None, None)),
    };
    let parent = match memory
        .doc
        .parent(node)
        .map_err(|_| MemoryCodecError::InvalidSnapshot)?
    {
        Parent::Root => None,
        Parent::Node(parent) => Some(parent.to_storage()),
    };
    Ok(json!({
        "node_id":node.to_storage(), "parent_id":parent,
        "kind":memory.doc.kind_of(node).ok_or(MemoryCodecError::InvalidSnapshot)?.as_str(),
        "start_utf8":range.0,"end_utf8":range.1,
        "content":koru_mcp_server::project_node_content(&content, &runs),
    }))
}

pub(in crate::bots) fn page(
    memory: &VerifiedMemory,
    fact: NodeId,
    position: MemoryDetailPosition,
    budget: DetailBudget,
) -> Result<MemoryDetailPage> {
    if budget.max_page_bytes == 0
        || budget.max_page_bytes > 16384
        || budget.continuation_reserve_bytes > 2048
    {
        return Err(MemoryCodecError::DetailTooLarge);
    }
    let nodes = walk(memory, fact)?;
    if position.node_ordinal > nodes.len()
        || (position.node_ordinal == nodes.len() && position.start_utf8 != 0)
    {
        return Err(MemoryCodecError::InvalidDetailPosition);
    }
    let mut current = position;
    let mut chunks = Vec::new();
    while current.node_ordinal < nodes.len() {
        let node = nodes[current.node_ordinal];
        let value = memory
            .doc
            .read_node_content_strict(node, TEXT_LIMIT)
            .map_err(strict_error)?;
        let text = value.content.as_text();
        let length = text.map_or(0, str::len);
        if current.start_utf8 > length
            || text.is_some_and(|t| !t.is_char_boundary(current.start_utf8))
        {
            return Err(MemoryCodecError::InvalidDetailPosition);
        }
        let after = MemoryDetailPosition {
            node_ordinal: current.node_ordinal + 1,
            start_utf8: 0,
        };
        if length > 0 && current.start_utf8 == length {
            current = after;
            continue;
        }
        // Raw text alone is a lower bound on JSON bytes. Never construct an
        // unbounded candidate just to discover that it cannot fit this page.
        if length - current.start_utf8 <= budget.max_page_bytes {
            chunks.push(chunk(memory, node, &value, current.start_utf8, length)?);
            if fits(&chunks, after.node_ordinal == nodes.len(), budget) {
                current = after;
                continue;
            }
            chunks.pop();
        }
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            let ends: Vec<usize> = text[current.start_utf8..]
                .char_indices()
                .map(|(at, c)| current.start_utf8 + at + c.len_utf8())
                .take_while(|end| {
                    *end - current.start_utf8 <= budget.max_page_bytes && *end < length
                })
                .collect();
            let mut low = 0;
            let mut high = ends.len();
            while low < high {
                let mid = low + (high - low) / 2;
                chunks.push(chunk(memory, node, &value, current.start_utf8, ends[mid])?);
                let fitting = fits(&chunks, false, budget);
                chunks.pop();
                if fitting { low = mid + 1 } else { high = mid }
            }
            if low > 0 {
                let end = ends[low - 1];
                chunks.push(chunk(memory, node, &value, current.start_utf8, end)?);
                current.start_utf8 = end;
            }
        }
        if chunks.is_empty() {
            return Err(MemoryCodecError::DetailTooLarge);
        }
        return Ok(MemoryDetailPage {
            chunks,
            complete: false,
            next: Some(current),
        });
    }
    if !fits(&chunks, true, budget) {
        return Err(MemoryCodecError::DetailTooLarge);
    }
    Ok(MemoryDetailPage {
        chunks,
        complete: true,
        next: None,
    })
}

#[cfg(test)]
mod tests;
