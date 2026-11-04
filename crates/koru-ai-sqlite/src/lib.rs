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
pub use db::{OpenError, SCHEMA_VERSION};
pub use repository::{KnownAttachmentDirs, RemovedAttachment, Repository};

pub const DATABASE_FILE: &str = "ai.db";
