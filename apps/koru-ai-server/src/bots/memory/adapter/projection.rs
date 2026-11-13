// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::bots::memory::{MemoryIndex, detail, history::VerifiedMemory};
use serde::Deserialize;

const PAGE_BYTES: usize = 4096;

pub(super) fn verify_index(
    index: &MemoryIndex,
    nodes: &[BotMemoryNodeId],
    omitted: u64,
    invalid: u64,
    digest: [u8; 32],
) -> Result<()> {
    if index.nodes.len() != nodes.len()
        || index
            .nodes
            .iter()
            .zip(nodes)
            .any(|(a, b)| a.as_uuid() != b.as_uuid())
        || index.omitted_count != omitted
        || index.invalid_count != invalid
        || index.digest != digest
    {
        return Err(MemoryAuthorityError::CaptureUnavailable);
    }
    Ok(())
}

fn encoded_len(value: &impl serde::Serialize) -> Result<usize> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|_| BotError::Invalid.into())
}

pub(super) fn page(
    memory: &VerifiedMemory,
    position: MemoryReadPosition,
) -> Result<MemoryPageResult> {
    match position {
        MemoryReadPosition::List { after, limit } => list(memory, after, limit),
        MemoryReadPosition::Detail {
            fact_id,
            node_ordinal,
            start_utf8,
        } => detail(memory, fact_id, node_ordinal, start_utf8),
    }
}

fn list(memory: &VerifiedMemory, after: u32, limit: u16) -> Result<MemoryPageResult> {
    if !(1..=BOT_MEMORY_LIST_LIMIT).contains(&limit) || after as usize > memory.source.facts.len() {
        return Err(BotError::Invalid.into());
    }
    let index = index::encode_index(&memory.source).map_err(codec)?;
    let counts = BotMemoryCounts {
        valid: memory.source.facts.len() as u32,
        omitted_from_prompt: index.omitted_count as u32,
        invalid: memory.source.invalid_count as u32,
        not_indexed: memory.source.not_indexed_count as u32,
    };
    let mut items = Vec::new();
    let mut end = after as usize;
    for fact in memory.source.facts.iter().skip(end).take(limit as usize) {
        items.push(BotMemoryFactSummary {
            fact_id: BotMemoryNodeId::from_uuid(fact.node.as_uuid()),
            summary: fact.summary.clone(),
        });
        // Reserve the full canonical unescaped token, even if this particular
        // page would complete. This is a conservative, stable prefix budget.
        let measured = BotMemoryListPage::Observed {
            page: memory.source.page.clone(),
            version: memory.source.version,
            counts: counts.clone(),
            items: items.clone(),
            complete: false,
            continuation: Some("a".repeat(BOT_MEMORY_ISSUED_CURSOR_BYTES)),
        };
        if encoded_len(&measured)? > PAGE_BYTES {
            items.pop();
            break;
        }
        end += 1;
    }
    if end == after as usize && end < memory.source.facts.len() {
        return Err(BotError::ContextTooLarge.into());
    }
    let complete = end == memory.source.facts.len();
    Ok(MemoryPageResult::List {
        page: BotMemoryListPage::Observed {
            page: memory.source.page.clone(),
            version: memory.source.version,
            counts,
            items,
            complete,
            continuation: None,
        },
        next: (!complete).then_some(MemoryReadPosition::List {
            after: end as u32,
            limit,
        }),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodecChunk {
    node_id: BotMemoryNodeId,
    parent_id: Option<BotMemoryNodeId>,
    kind: String,
    start_utf8: Option<u32>,
    end_utf8: Option<u32>,
    content: serde_json::Value,
}

fn detail(
    memory: &VerifiedMemory,
    fact_id: BotMemoryNodeId,
    node_ordinal: u32,
    start_utf8: u32,
) -> Result<MemoryPageResult> {
    let fact = koru_core::NodeId::from_uuid(fact_id.as_uuid());
    if !memory
        .source
        .facts
        .iter()
        .any(|candidate| candidate.node == fact)
    {
        return Err(MemoryAuthorityError::FactNotFound);
    }
    let shell = BotMemoryDetailPage::Observed {
        page: memory.source.page.clone(),
        version: memory.source.version,
        fact_id,
        chunks: Vec::new(),
        complete: false,
        continuation: Some("a".repeat(BOT_MEMORY_ISSUED_CURSOR_BYTES)),
    };
    let codec_shell = serde_json::json!({
        "chunks":[], "complete":false, "continuation":"a".repeat(BOT_MEMORY_ISSUED_CURSOR_BYTES),
    });
    let overhead = encoded_len(&shell)?.saturating_sub(encoded_len(&codec_shell)?);
    // Wrapping flat start/end fields in `range` adds ten bytes per text chunk.
    // Every raw chunk contains at least this node-id object, so this bounds the
    // maximum count without guessing from typical document shapes.
    let minimum_chunk_bytes = encoded_len(&serde_json::json!({"node_id":uuid::Uuid::nil()}))?;
    let range_framing = (PAGE_BYTES / minimum_chunk_bytes) * 10;
    let page = detail::page(
        memory,
        fact,
        detail::MemoryDetailPosition {
            node_ordinal: node_ordinal as usize,
            start_utf8: start_utf8 as usize,
        },
        detail::DetailBudget {
            max_page_bytes: PAGE_BYTES
                .checked_sub(overhead + range_framing)
                .ok_or(MemoryAuthorityError::DetailTooLarge)?,
            continuation_reserve_bytes: BOT_MEMORY_ISSUED_CURSOR_BYTES,
        },
    )
    .map_err(codec)?;
    let chunks = page
        .chunks
        .into_iter()
        .map(|chunk| {
            let chunk: CodecChunk = serde_json::from_value(chunk).map_err(|_| BotError::Invalid)?;
            let range = match (chunk.start_utf8, chunk.end_utf8) {
                (Some(start_utf8), Some(end_utf8)) if start_utf8 <= end_utf8 => {
                    Some(BotMemoryTextRange {
                        start_utf8,
                        end_utf8,
                    })
                }
                (None, None) => None,
                _ => return Err(BotError::Invalid),
            };
            Ok(BotMemoryDetailChunk {
                node_id: chunk.node_id,
                parent_id: chunk.parent_id,
                kind: chunk.kind,
                range,
                content: chunk.content,
            })
        })
        .collect::<std::result::Result<Vec<_>, BotError>>()?;
    let mut result = BotMemoryDetailPage::Observed {
        page: memory.source.page.clone(),
        version: memory.source.version,
        fact_id,
        chunks,
        complete: page.complete,
        continuation: (!page.complete).then(|| "a".repeat(BOT_MEMORY_ISSUED_CURSOR_BYTES)),
    };
    if encoded_len(&result)? > PAGE_BYTES {
        return Err(MemoryAuthorityError::DetailTooLarge);
    }
    let BotMemoryDetailPage::Observed { continuation, .. } = &mut result else {
        unreachable!()
    };
    *continuation = None;
    Ok(MemoryPageResult::Detail {
        page: result,
        next: page.next.map(|next| MemoryReadPosition::Detail {
            fact_id,
            node_ordinal: next.node_ordinal as u32,
            start_utf8: next.start_utf8 as u32,
        }),
    })
}
