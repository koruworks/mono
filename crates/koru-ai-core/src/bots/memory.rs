// SPDX-License-Identifier: MPL-2.0

//! Ordinary Memory projections. These values carry data, never Store or Run authority.
use super::*;
use koru_delegate_core::DocumentVersionId;

/// One internal exposure gate for worker dispatch, UI scheduling/controls,
/// native catalog and native direct calls. Only the lead's complete integration
/// checkpoint may enable it; this is not an environment or user setting.
pub const BOT_MEMORY_AVAILABLE: bool = true;
/// Writes require the separately verified lazy-creation and native-effect path.
/// Enabling read-only Memory must never enable these private/public write routes.
pub const BOT_MEMORY_WRITES_AVAILABLE: bool = true;
pub const BOT_MEMORY_CURSOR_BYTES: usize = 2048;
/// Issued canonical typed tokens are smaller than the accepted input ceiling.
/// The issuer's maximum-shape test proves this bound; projections reserve it.
pub const BOT_MEMORY_ISSUED_CURSOR_BYTES: usize = 1024;
pub const BOT_MEMORY_LIST_LIMIT: u16 = 20;
pub const BOT_MEMORY_INDEX_BYTES: usize = 8192;
pub const BOT_MEMORY_RESPONSE_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryAuthorityError {
    Authority(BotError),
    CaptureUnavailable,
    HistoryUnavailable,
    DetailUnsupported,
    DetailTooLarge,
    FactNotFound,
}
impl From<BotError> for MemoryAuthorityError {
    fn from(value: BotError) -> Self {
        Self::Authority(value)
    }
}
impl MemoryAuthorityError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Authority(error) => error.code(),
            Self::CaptureUnavailable => "memory_capture_unavailable",
            Self::HistoryUnavailable => "memory_history_unavailable",
            Self::DetailUnsupported => "memory_detail_unsupported",
            Self::DetailTooLarge => "memory_detail_too_large",
            Self::FactNotFound => "memory_fact_not_found",
        }
    }
}

/// A definite creation observation. Transport/cancellation failures remain an
/// error: they cannot establish whether an already queued Store write happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreparedMemoryPage {
    Confirmed(BotMemoryConfirmation),
    Disagreement(BotMemoryRefusal),
}

/// Trusted adapter input. An exact version is supplied only by a durable
/// capture or a successfully authenticated continuation, never a public ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemorySnapshot {
    Current,
    Exact(DocumentVersionId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryReadPosition {
    List {
        after: u32,
        limit: u16,
    },
    Detail {
        fact_id: BotMemoryNodeId,
        node_ordinal: u32,
        start_utf8: u32,
    },
}

/// Cursor-free adapter output. Only the worker can sign the next position;
/// public continuations in these data pages are always initially None.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryPageResult {
    List {
        page: BotMemoryListPage,
        next: Option<MemoryReadPosition>,
    },
    Detail {
        page: BotMemoryDetailPage,
        next: Option<MemoryReadPosition>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotMemoryStatus {
    pub bot_id: BotId,
    pub revision: u64,
    pub state: BotMemoryStatusState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotMemoryStatusState {
    NotStarted {
        can_create: bool,
    },
    Candidate {
        creation_request: BotRequestId,
        candidate: BotPage,
        state: BotMemoryState,
        observed_updated_at_ms: i64,
        refusal: Option<BotMemoryRefusal>,
        can_retry: bool,
        can_abandon: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotMemoryFactSummary {
    pub fact_id: BotMemoryNodeId,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotMemoryCounts {
    pub valid: u32,
    pub omitted_from_prompt: u32,
    pub invalid: u32,
    pub not_indexed: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotMemoryListPage {
    Absent {},
    Observed {
        page: BotPage,
        version: DocumentVersionId,
        counts: BotMemoryCounts,
        items: Vec<BotMemoryFactSummary>,
        complete: bool,
        continuation: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotMemoryDetailPage {
    Absent {},
    Observed {
        page: BotPage,
        version: DocumentVersionId,
        fact_id: BotMemoryNodeId,
        chunks: Vec<BotMemoryDetailChunk>,
        complete: bool,
        continuation: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotMemoryDetailChunk {
    pub node_id: BotMemoryNodeId,
    pub parent_id: Option<BotMemoryNodeId>,
    pub kind: String,
    pub range: Option<BotMemoryTextRange>,
    /// The shared MCP content projection, including typed attributes and runs.
    /// It is not an editable complete node or a source of execution authority.
    pub content: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotMemoryTextRange {
    pub start_utf8: u32,
    pub end_utf8: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryView {
    Captured,
    Current,
}

/// Private worker transport input, not a public owner command or permission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryReadOperation {
    List {
        view: MemoryView,
        cursor: Option<String>,
        limit: u16,
    },
    Detail {
        view: MemoryView,
        fact_id: BotMemoryNodeId,
        cursor: Option<String>,
    },
}

impl MemoryReadOperation {
    /// Bounds only. A cursor still needs its keyed proof and current authority.
    pub fn validate_bounds(&self) -> Result<(), BotError> {
        let cursor = match self {
            Self::List { cursor, limit, .. } => {
                if !(1..=BOT_MEMORY_LIST_LIMIT).contains(limit) {
                    return Err(BotError::Invalid);
                }
                cursor
            }
            Self::Detail { cursor, .. } => cursor,
        };
        if cursor.as_ref().is_some_and(|cursor| {
            cursor.is_empty()
                || cursor.len() > BOT_MEMORY_CURSOR_BYTES
                || !cursor
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        }) {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
}

/// An off-writer observation to compare with the canonical binding before capture.
/// Constructing this value does not validate Store history or grant access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedMemoryCapture {
    pub owner: BotOwner,
    pub bot: BotId,
    pub creation_request: BotRequestId,
    pub page: BotPage,
    pub version: DocumentVersionId,
    pub nodes: Vec<BotMemoryNodeId>,
    pub omitted_count: u64,
    pub invalid_count: u64,
    pub index_digest: [u8; 32],
}

impl PreparedMemoryCapture {
    /// Identity/shape comparison, not the adapter's snapshot or digest proof.
    pub fn validate_for_binding(&self, binding: &BotMemoryBinding) -> Result<(), BotError> {
        if self.owner != binding.owner
            || self.bot != binding.bot
            || self.creation_request != binding.creation_request
            || self.page != binding.page
            || self.nodes.len() > BOT_MEMORY_INDEX_LIMIT
            || self
                .nodes
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.nodes.len()
            || self.omitted_count > i64::MAX as u64
            || self.invalid_count > i64::MAX as u64
            || ![
                self.owner.user_scope.as_uuid(),
                self.owner.device.as_uuid(),
                self.bot.as_uuid(),
                self.creation_request.as_uuid(),
                self.page.workspace.as_uuid(),
                self.page.document.as_uuid(),
                self.version.as_uuid(),
            ]
            .into_iter()
            .all(super::library::valid_id)
        {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
}

/// Transient encoder output. The adapter must verify exact history and digest;
/// the writer must compare the entire capture with its durable submission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMemoryIndex {
    pub capture: BotLibraryCapture,
    pub bytes: Vec<u8>,
}

impl VerifiedMemoryIndex {
    /// Shape only; this deliberately cannot replace the adapter's BLAKE3 check.
    pub fn validate_shape(&self, home: WorkspaceId) -> Result<(), BotError> {
        self.capture.validate(home)?;
        if self.bytes.len() > BOT_MEMORY_INDEX_BYTES
            || std::str::from_utf8(&self.bytes).is_err()
            || matches!(self.capture.memory, BotCapturedMemory::Absent {}) && !self.bytes.is_empty()
            || matches!(self.capture.memory, BotCapturedMemory::Captured { .. })
                && self.bytes.is_empty()
        {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
