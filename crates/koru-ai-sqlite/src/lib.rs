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
    BOT_DECISION_NEEDED_YOU, BotBootSnapshot, BotCreationScan, BotDecisionCard, BotDecisionClosure,
    BotDecisionReplayRollover, BotHold, BotNativeDecisionLink, HistoricalBotRun,
    KnownAttachmentDirs, LifecycleCapture, LifecycleMember, LifecycleMemberDraft,
    LifecycleOperation, LifecycleRoutineAnswer, ParkedCursor, ParkedForOwner, RemovedAttachment,
    Repository, RoutineAccessRecoveryPage, RoutineDetailSnapshot, RoutineListCheckpoint,
    RoutineListSnapshot, RoutineReadSubject, RoutineToolSubject,
};

pub const DATABASE_FILE: &str = "ai.db";
