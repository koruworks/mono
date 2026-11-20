// SPDX-License-Identifier: MPL-2.0

//! The Bots engine's device-local authority. Workers reach it over IPC;
//! this crate never opens a Store or starts a coding agent on a request thread.

pub mod attachments;
pub mod authority;
pub mod bot_database;
pub mod bot_worker;
pub mod capacity;
pub mod carrier;
pub mod configuration;
pub mod creation;
mod database;
pub mod door;
mod error;
pub mod folders;
pub mod launcher;
mod limits;
pub mod messages;
pub mod observations;
pub mod operations;
pub mod own_tools;
mod ownership;
pub mod person;
pub mod questions;
pub mod routines;
#[cfg(any(test, feature = "provider-runtime"))]
pub mod sessions;
pub mod worker;
pub mod workspace;

pub use database::{DATABASE_NAME, Database, Diagnostic, Status, inspect};
pub use error::Error;
pub use ownership::claim_root;
