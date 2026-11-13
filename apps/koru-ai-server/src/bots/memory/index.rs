// SPDX-License-Identifier: MPL-2.0

use super::*;
use serde::Serialize;

const PREFIX: &[u8] = b"\n<koru-memory-index-v1>\n";
const SUFFIX: &[u8] = b"\n</koru-memory-index-v1>\n";

// Field declaration order, JSON escaping, and outer framing are format 1.
#[derive(Serialize)]
struct EncodedIndex<'a> {
    version: u32,
    page: &'a BotPage,
    history: String,
    facts: &'a [EncodedFact<'a>],
    omitted_count: u64,
    invalid_count: u64,
    not_indexed_count: u64,
}
#[derive(Serialize)]
struct EncodedFact<'a> {
    id: String,
    summary: &'a str,
}

fn json(value: &impl Serialize) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| MemoryCodecError::InvalidSnapshot)
}

pub(in crate::bots) fn encode_index(source: &MemoryIndexSource) -> Result<MemoryIndex> {
    if source.facts.len() > NODE_LIMIT
        || source.invalid_count > NODE_LIMIT as u64
        || source.not_indexed_count > NODE_LIMIT as u64
    {
        return Err(MemoryCodecError::InvalidSnapshot);
    }
    let mut encoded = EncodedIndex {
        version: 1,
        page: &source.page,
        history: source.version.as_uuid().to_string(),
        facts: &[],
        omitted_count: u64::MAX,
        invalid_count: u64::MAX,
        not_indexed_count: u64::MAX,
    };
    let mut reserved = PREFIX.len() + json(&encoded)?.len() + SUFFIX.len();
    if reserved > INDEX_LIMIT {
        return Err(MemoryCodecError::TooLarge(MemoryBudget::Index));
    }
    let mut facts = Vec::new();
    for fact in &source.facts {
        if fact.summary.is_empty() || fact.summary.len() > SUMMARY_LIMIT {
            return Err(MemoryCodecError::InvalidSnapshot);
        }
        let entry = EncodedFact {
            id: fact.node.as_uuid().to_string(),
            summary: &fact.summary,
        };
        let added = json(&entry)?.len() + usize::from(!facts.is_empty());
        if facts.len() == FACT_LIMIT || reserved + added > INDEX_LIMIT {
            break;
        }
        reserved += added;
        facts.push(entry);
    }
    encoded.facts = &facts;
    encoded.omitted_count = (source.facts.len() - facts.len()) as u64;
    encoded.invalid_count = source.invalid_count;
    encoded.not_indexed_count = source.not_indexed_count;
    let mut bytes = PREFIX.to_vec();
    bytes.extend(json(&encoded)?);
    bytes.extend_from_slice(SUFFIX);
    if bytes.len() > INDEX_LIMIT {
        return Err(MemoryCodecError::TooLarge(MemoryBudget::Index));
    }
    let digest = *blake3::hash(&bytes).as_bytes();
    Ok(MemoryIndex {
        nodes: source.facts[..facts.len()]
            .iter()
            .map(|fact| fact.node)
            .collect(),
        omitted_count: encoded.omitted_count,
        invalid_count: source.invalid_count,
        not_indexed_count: source.not_indexed_count,
        bytes,
        digest,
    })
}

pub(in crate::bots) fn verify_capture(
    expected: &MemoryIndexExpectation,
    snapshot: Option<&[u8]>,
) -> Result<MemoryIndex> {
    match (expected, snapshot) {
        (MemoryIndexExpectation::Absent { digest }, None)
            if *digest == *blake3::hash(&[]).as_bytes() =>
        {
            Ok(MemoryIndex {
                nodes: Vec::new(),
                omitted_count: 0,
                invalid_count: 0,
                not_indexed_count: 0,
                bytes: Vec::new(),
                digest: *digest,
            })
        }
        (
            MemoryIndexExpectation::Captured {
                page,
                version,
                nodes,
                omitted,
                invalid,
                digest,
            },
            Some(snapshot),
        ) => {
            let value = encode_index(&observe(page.clone(), *version, snapshot)?)?;
            if value.nodes != *nodes
                || value.omitted_count != *omitted
                || value.invalid_count != *invalid
                || value.digest != *digest
            {
                return Err(MemoryCodecError::InvalidCapture);
            }
            Ok(value)
        }
        _ => Err(MemoryCodecError::InvalidCapture),
    }
}
