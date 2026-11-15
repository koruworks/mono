// SPDX-License-Identifier: MPL-2.0

//! The unchanged fact vocabulary lives in the neutral owner so a journaled
//! decision projection can retain exact stable publication bytes before apply.
//! Runtime remains the semantic producer and keeps its existing import paths.

pub use koru_delegate_core::{RunFact, UsageReport};
