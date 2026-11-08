// SPDX-License-Identifier: MPL-2.0

//! Stable, surface-neutral vocabulary for Koru terminal sessions.
//!
//! This crate describes sessions; it does not run, transport, or render them.
//! In particular it names no PTY, emulator, process, Agent, Store, gateway,
//! window, or Vello type. Provider-native identifiers stop at runtime adapters.
//!
//! Closed, behavior-bearing enums reject unknown variants. Capabilities and
//! error codes are deliberately extensible tokens: an unknown capability is
//! retained but not enabled unless both peers advertise it, and an unknown
//! error code remains an error.

mod agent_commands;
mod approval;
mod attachment;
mod command;
mod delegation;
mod error;
mod event;
mod ids;
mod models;
mod modes;
mod negotiation;
mod origin;
mod profile;
mod project;
mod questions;
mod readiness;
pub mod schema;
mod session;
mod terminal;
mod token;
mod tool;

pub use agent_commands::*;
pub use approval::*;
pub use attachment::*;
pub use command::*;
pub use delegation::*;
pub use error::*;
pub use event::*;
pub use ids::*;
pub use models::*;
pub use modes::*;
pub use negotiation::*;
pub use origin::*;
pub use profile::*;
pub use project::*;
pub use questions::*;
pub use readiness::*;
pub use session::*;
pub use terminal::*;
pub use token::*;
pub use tool::*;

#[cfg(test)]
mod tests;
