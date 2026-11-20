// SPDX-License-Identifier: MPL-2.0

use crate::MessageId;
use serde::{Deserialize, Serialize};

/// A bounded read-only excerpt of an exact, completed collaboration reply.
/// The full response remains in its source conversation; this is never stored.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BotReplyPreview {
    pub message: MessageId,
    pub markdown: String,
    pub truncated: bool,
}

impl BotReplyPreview {
    pub const MAX_BYTES: usize = 16 * 1024;

    pub fn new(message: MessageId, markdown: &str) -> Option<Self> {
        if markdown.trim().is_empty() {
            return None;
        }
        let mut end = markdown.len().min(Self::MAX_BYTES);
        while !markdown.is_char_boundary(end) {
            end -= 1;
        }
        Some(Self {
            message,
            markdown: markdown[..end].to_owned(),
            truncated: end < markdown.len(),
        })
    }
}

impl std::fmt::Debug for BotReplyPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotReplyPreview")
            .field("message", &self.message)
            .field("truncated", &self.truncated)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
