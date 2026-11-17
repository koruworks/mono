// SPDX-License-Identifier: MPL-2.0

//! The visible local browser route, lower half (M7, piece N).
//!
//! **The route is not qualified.** Nothing here has been run against a
//! browser. These modules are the instrument the qualification needs, and
//! `examples/qualify.rs` is the program that uses them. The port the rest of
//! Koru will see, the session, the extraction script and the fakes are held
//! until part A of that qualification has passed.

pub mod fixture;
pub mod installed;
pub mod launch;
pub mod origin;
pub mod protocol;
pub mod transport;
