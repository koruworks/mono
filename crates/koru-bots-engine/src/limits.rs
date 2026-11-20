// SPDX-License-Identifier: MPL-2.0

//! Bounds shared by the engine's commands, tool door and history pages.

/// A complete command or response fits within one MiB.
pub const MAX_PAYLOAD_BYTES: usize = 1_048_576;
/// A history page carries at most 64 entries and half a MiB of content.
pub const MAX_PAGE_ENTRIES: usize = 64;
pub const MAX_PAGE_BYTES: usize = 524_288;
