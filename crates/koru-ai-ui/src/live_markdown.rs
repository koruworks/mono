// SPDX-License-Identifier: MPL-2.0

//! **A streaming answer drawn as Markdown while it arrives.**
//!
//! Each block of the answer — a paragraph, a heading, a list item, a quote,
//! a table, a fenced block — is rendered as Markdown once it is closed:
//! once what follows it means nothing still to come can change it
//! (`koru_ai_core::cut_markdown`, the cut a finished reply is stored by).
//! Only the open block at the end is drawn as plain text, so an unclosed
//! fence keeps everything after it plain until it closes.
//!
//! This runs on every text delta, so it never starts over: the closed
//! blocks are kept, each under an id of its own that the layout cache
//! reuses its layout by, and a delta cuts only the text after the last of
//! them. The ids are this surface's, in memory; the finished reply is
//! drawn from its stored blocks, cut again from the whole text, exactly as
//! a reply that never streamed.

use koru_ai_core::{MessageBlockId, MessageId};
use koru_draw::text::TextEngine;
use koru_draw::theme::role;

use crate::layout_cache::BlockLayout;

/// One running answer's closed blocks, kept between deltas.
pub(crate) struct LiveAnswer {
    /// The reply, and where its answer starts in its draft: the answer
    /// starts over when either moves.
    key: Option<(MessageId, usize)>,
    /// The answer's text through its last closed block.
    closed_text: String,
    closed: Vec<(MessageBlockId, String)>,
    /// The open block's id: the same while it grows.
    tail: MessageBlockId,
}

impl Default for LiveAnswer {
    fn default() -> Self {
        Self {
            key: None,
            closed_text: String::new(),
            closed: Vec::new(),
            tail: MessageBlockId::new(),
        }
    }
}

impl LiveAnswer {
    /// The blocks of `answer` — the text of `message`'s draft from byte
    /// `start` — the closed ones as Markdown and then, under
    /// [`Self::tail_id`], the open one as it stands.
    pub fn blocks(
        &mut self,
        message: MessageId,
        start: usize,
        answer: &str,
    ) -> Vec<(MessageBlockId, String)> {
        if self.key != Some((message, start)) || !answer.starts_with(self.closed_text.as_str()) {
            *self = Self {
                key: Some((message, start)),
                ..Self::default()
            };
        }
        let rest = &answer[self.closed_text.len()..];
        let mut cut_through = 0;
        for block in koru_ai_core::cut_markdown(rest) {
            if !block.closed {
                break;
            }
            self.closed.push((MessageBlockId::new(), block.markdown));
            cut_through = block.end;
        }
        self.closed_text.push_str(&rest[..cut_through]);
        let open = answer[self.closed_text.len()..].trim();
        let mut blocks = self.closed.clone();
        if !open.is_empty() {
            blocks.push((self.tail, open.to_owned()));
        }
        blocks
    }

    pub fn tail_id(&self) -> MessageBlockId {
        self.tail
    }
}

/// The open block, as the text it is so far.
pub(crate) fn plain_block(
    engine: &mut TextEngine,
    id: MessageBlockId,
    text: &str,
    width: f32,
) -> BlockLayout {
    BlockLayout {
        id,
        markdown: text.to_owned(),
        wrap_width: width,
        text: text.to_owned(),
        layout: engine.simple_layout_wrapped(text, role::ui(), width),
        links: Vec::new(),
        table: None,
        image: None,
    }
}

#[cfg(test)]
mod tests;
