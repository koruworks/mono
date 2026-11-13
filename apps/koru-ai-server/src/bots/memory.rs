// SPDX-License-Identifier: MPL-2.0

//! Pure Memory format 1. No Store authority, durable capture, or public control.
//! See docs/features/bots/M3.P3.CODEC.PLAN.final.md for staged integration limits.
use koru_ai_core::bots::{BotMemoryIdentity, BotPage};
use koru_core::NodeId;
use koru_delegate_core::DocumentVersionId;

pub(super) mod detail;
pub(super) mod history;
pub(super) mod index;
mod observe;
pub(super) mod seed;
pub(super) use observe::observe;

const SNAPSHOT_LIMIT: usize = 16 * 1024 * 1024;
const NODE_LIMIT: usize = 50_000;
const DEPTH_LIMIT: usize = 256;
const TEXT_LIMIT: usize = 8 * 1024 * 1024;
const SUMMARY_LIMIT: usize = 1024;
const INDEX_LIMIT: usize = 8192;
const FACT_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MemoryBudget {
    EncodedSnapshot,
    RawNodes,
    Depth,
    Text,
    Index,
    HistoryMetadata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MemoryCodecError {
    InvalidSeed,
    InvalidSnapshot,
    UnsupportedSchema,
    TooLarge(MemoryBudget),
    InvalidCapture,
    UnsupportedDetail,
    InvalidDetailPosition,
    DetailTooLarge,
}
type Result<T> = std::result::Result<T, MemoryCodecError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MemorySummary {
    pub node: NodeId,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MemoryDiagnosticCode {
    InvalidSummary,
    NotIndexed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MemoryDiagnostic {
    pub node: NodeId,
    pub code: MemoryDiagnosticCode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MemoryIndexSource {
    pub page: BotPage,
    pub version: DocumentVersionId,
    pub facts: Vec<MemorySummary>,
    pub diagnostics: Vec<MemoryDiagnostic>,
    pub invalid_count: u64,
    pub not_indexed_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MemoryIndex {
    pub nodes: Vec<NodeId>,
    pub omitted_count: u64,
    pub invalid_count: u64,
    pub not_indexed_count: u64,
    pub bytes: Vec<u8>,
    pub digest: [u8; 32],
}

/// This is separate from the durable authenticated Memory capture DTO.
/// Authentication and exact-history selection belong to the future adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MemoryIndexExpectation {
    Absent {
        digest: [u8; 32],
    },
    Captured {
        page: BotPage,
        version: DocumentVersionId,
        nodes: Vec<NodeId>,
        omitted: u64,
        invalid: u64,
        digest: [u8; 32],
    },
}

fn bounded_import(bytes: &[u8]) -> Result<koru_core::WorkspaceDoc> {
    if bytes.len() > SNAPSHOT_LIMIT {
        return Err(MemoryCodecError::TooLarge(MemoryBudget::EncodedSnapshot));
    }
    if bytes.is_empty() {
        return Err(MemoryCodecError::InvalidSnapshot);
    }
    koru_core::WorkspaceDoc::from_snapshot(bytes).map_err(|_| MemoryCodecError::InvalidSnapshot)
}

#[cfg(test)]
mod tests;
