// SPDX-License-Identifier: MPL-2.0

//! The routes, one module per subject.
//!
//! Split by what they are *about* rather than by verb, so the file you open to
//! change workspace behaviour is the one named after workspaces.

pub mod admin;
pub mod attributes;
pub mod bundles;
pub mod catalog;
pub mod debug;
pub mod meta;
pub mod reader;
pub mod readings;
pub mod reminders;
pub mod search;
pub mod status;
pub mod sync;
pub mod tasks;
pub mod welcome;
pub mod workspaces;
