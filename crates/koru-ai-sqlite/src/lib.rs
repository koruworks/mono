// SPDX-License-Identifier: MPL-2.0

//! Sole owner of `ai.db` access and schema migration.
//!
//! Provider payloads and Store document bodies never cross this boundary. The
//! repository persists Koru-owned chat history, provenance, attempt metadata,
//! and the disposable message-block search projection.

mod db;
mod repository;

#[cfg(feature = "fixtures")]
pub use db::write_at_schema;
pub use db::{OpenError, SCHEMA_VERSION, SUPERSEDED_DEVELOPMENT_SCHEMA};
pub use repository::{
    BOT_DECISION_NEEDED_YOU, BotBootSnapshot, BotChildReplayRollover, BotCreationScan,
    BotDecisionCard, BotDecisionClosure, BotDecisionReplayRollover, BotHold, BotNativeDecisionLink,
    BotTeamPauseRow, CLIENT_RECEIPT_KEEP_MS, CLIENT_RECEIPT_PRUNE_BATCH, ClientBotRequest,
    ClientOutcome, ClientReceipt, ClientReceiptBegin, FEED_RECORD_TABLES, FEED_STARTS_KEPT,
    FeedBody, FeedBotHome, FeedChat, FeedCounts, FeedItem, FeedKind, FeedMarker, FeedMessage,
    FeedScan, HistoricalBotRun, KnownAttachmentDirs, LifecycleCapture, LifecycleMember,
    LifecycleMemberDraft, LifecycleOperation, LifecycleRoutineAnswer, ParkedCursor, ParkedForOwner,
    RemovedAttachment, Repository, RoutineAccessRecoveryPage, RoutineDetailSnapshot,
    RoutineListCheckpoint, RoutineListSnapshot, RoutineReadSubject, RoutineToolSubject,
    TeamPauseChange,
};

pub const DATABASE_FILE: &str = "ai.db";
