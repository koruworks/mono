// SPDX-License-Identifier: MPL-2.0

//! The Bots engine's device-local authority. Workers reach it over IPC;
//! this crate never opens a Store or starts a coding agent on a request thread.

pub mod bot_database;
pub mod bot_worker;
pub mod capacity;
mod database;
mod error;
pub mod launcher;
pub mod messages;
pub mod sessions;
mod ownership;
pub mod worker;

pub use database::{DATABASE_NAME, Database, Diagnostic, Status, inspect};
pub use error::Error;
pub use ownership::{claim_root, legacy_allowed};

/// One compile-time choice shared by every consumer in this build. This is
/// deliberately not a runtime UI flag. M5 removes the legacy alternative.
pub const ENABLED: bool = cfg!(feature = "active");
