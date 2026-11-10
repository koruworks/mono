// SPDX-License-Identifier: MPL-2.0

use std::collections::HashMap;

use koru_ai_core::{ChatRole, MessageBlockId, MessageId, MessageState, SourceLink};
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Brush};
use parley::Layout;

use super::{
    ImageSpec,
    attachments::Tile,
    sources::{self, SourcesLayout},
};

pub(crate) struct MessageLayout {
    pub id: Option<MessageId>,
    pub top: f32,
    pub role: ChatRole,
    pub state: MessageState,
    pub markdown: String,
    pub display: String,
    /// Where `display` carries a link — a document chip in a message the
    /// person sent, drawn as its title (`crate::doc_links`).
    pub links: Vec<koru_markdown::LinkSpan>,
    pub wrap_width: f32,
    pub layout: Layout<Brush>,
    pub usage_text: Option<String>,
    pub usage: Option<Layout<Brush>>,
    pub sources: Option<SourcesLayout>,
    pub blocks: Vec<BlockLayout>,
    /// Pictures, files, and folders sent with a message, drawn as one-line
    /// pills under its text with widths measured during layout.
    pub attachments: Vec<Tile>,
}

pub(crate) struct BlockLayout {
    pub id: MessageBlockId,
    pub markdown: String,
    pub wrap_width: f32,
    pub text: String,
    pub layout: Layout<Brush>,
    pub links: Vec<koru_markdown::LinkSpan>,
    pub table: Option<koru_markdown::table::TableGrid>,
    pub image: Option<ImageSpec>,
}

/// How tall a reply's picture box is; the picture is fitted inside it. A
/// picture in a call's detail is never taller (`crate::tool_output::picture`).
pub(crate) const IMAGE_BOX_H: f32 = 260.0;

impl BlockLayout {
    pub fn height(&self) -> f32 {
        if self.image.is_some() {
            return IMAGE_BOX_H;
        }
        self.table
            .as_ref()
            .map_or_else(|| self.layout.height(), |table| table.height)
    }

    pub fn width(&self) -> f32 {
        if self.image.is_some() {
            return 420.0;
        }
        self.table
            .as_ref()
            .map_or_else(|| self.layout.width(), |table| table.width)
    }

    /// The link under `(x, y)`, measured from the block's top-left — in its
    /// text, or in one of its table's cells (koru-ai D95). Both answer
    /// through `koru_markdown`, so no kind of block finds links its own way.
    pub fn link_at(&self, x: f32, y: f32) -> Option<koru_markdown::LinkHit<'_>> {
        if self.image.is_some() {
            return None;
        }
        match &self.table {
            Some(table) => koru_markdown::table::link_at(table, x, y),
            None => koru_markdown::link_at(&self.text, &self.layout, &self.links, x, y),
        }
    }
}

/// The byte of a layout's text under a point (`koru_markdown::byte_at`).
pub(crate) use koru_markdown::byte_at;

pub(crate) struct MessageInput {
    pub id: MessageId,
    pub role: ChatRole,
    pub state: MessageState,
    pub markdown: String,
    pub display: String,
    pub links: Vec<koru_markdown::LinkSpan>,
    pub wrap_width: f32,
    pub usage_text: Option<String>,
    pub sources: Vec<SourceLink>,
    pub blocks: Vec<(MessageBlockId, String)>,
    pub attachments: Vec<Tile>,
}

#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub(crate) struct RebuildStats {
    pub messages_reused: usize,
    pub messages_laid_out: usize,
    pub blocks_reused: usize,
    pub blocks_laid_out: usize,
}

impl std::ops::AddAssign for RebuildStats {
    fn add_assign(&mut self, other: Self) {
        self.messages_reused += other.messages_reused;
        self.messages_laid_out += other.messages_laid_out;
        self.blocks_reused += other.blocks_reused;
        self.blocks_laid_out += other.blocks_laid_out;
    }
}

pub(crate) fn reconcile(
    engine: &mut TextEngine,
    old: Vec<MessageLayout>,
    inputs: Vec<MessageInput>,
    mut render: impl FnMut(&mut TextEngine, MessageBlockId, &str, f32) -> BlockLayout,
) -> (Vec<MessageLayout>, RebuildStats) {
    let mut old = old
        .into_iter()
        .filter_map(|message| message.id.map(|id| (id, message)))
        .collect::<HashMap<_, _>>();
    let mut output = Vec::with_capacity(inputs.len());
    let mut stats = RebuildStats::default();
    for input in inputs {
        let prior = old.remove(&input.id);
        if prior
            .as_ref()
            .is_some_and(|message| reusable(message, &input))
        {
            let message = prior.unwrap();
            stats.messages_reused += 1;
            stats.blocks_reused += message.blocks.len();
            output.push(message);
            continue;
        }

        let mut prior_blocks = prior
            .map(|message| {
                message
                    .blocks
                    .into_iter()
                    .map(|block| (block.id, block))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let mut blocks = Vec::with_capacity(input.blocks.len());
        for (id, markdown) in &input.blocks {
            if let Some(block) = prior_blocks.remove(id).filter(|block| {
                block.markdown == *markdown && (block.wrap_width - input.wrap_width).abs() <= 0.5
            }) {
                stats.blocks_reused += 1;
                blocks.push(block);
            } else {
                stats.blocks_laid_out += 1;
                blocks.push(render(engine, *id, markdown, input.wrap_width));
            }
        }
        stats.messages_laid_out += 1;
        let usage = input
            .usage_text
            .as_ref()
            .map(|text| engine.simple_layout_wrapped(text, theme::role::hint(), input.wrap_width));
        let sources = sources::layout_sources(engine, &input.sources, input.wrap_width);
        // A streaming answer is drawn from its blocks; laying all of its
        // text out again on every delta is what the blocks spare.
        let layout = if input.state == MessageState::Pending && !input.blocks.is_empty() {
            crate::chat_text_layout(engine, "", input.wrap_width)
        } else {
            display_layout(engine, &input.display, &input.links, input.wrap_width)
        };
        output.push(MessageLayout {
            id: Some(input.id),
            top: 0.0,
            role: input.role,
            state: input.state,
            markdown: input.markdown,
            display: input.display.clone(),
            layout,
            links: input.links,
            wrap_width: input.wrap_width,
            usage_text: input.usage_text,
            usage,
            sources,
            blocks,
            attachments: input.attachments,
        });
    }
    (output, stats)
}

/// A message's own text, wrapped, with any links in the link color — the
/// token a chip was sent as reads as the document's title
/// (`crate::doc_links::titled`).
pub(crate) fn display_layout(
    engine: &mut TextEngine,
    display: &str,
    links: &[koru_markdown::LinkSpan],
    wrap_width: f32,
) -> Layout<Brush> {
    // A quote the person sent (D88) is set behind the reply's blockquote bar
    // (`crate::quote_pills::quote_bars`) and drawn in the dim ink, so what
    // they quoted reads apart from what they said about it.
    let mut quoted = Vec::new();
    let mut start = 0;
    for line in display.split_inclusive('\n') {
        if line.starts_with(crate::quote_pills::QUOTE_BAR.trim_end()) {
            quoted.push(start..start + line.trim_end_matches('\n').len());
        }
        start += line.len();
    }
    if links.is_empty() && quoted.is_empty() {
        return crate::chat_text_layout(engine, display, wrap_width);
    }
    let style = theme::role::ui();
    let ranges: Vec<koru_draw::text::RangeStyle> = quoted
        .into_iter()
        .map(|range| koru_draw::text::RangeStyle {
            range,
            color: Some(theme::p().text_dim),
            strikethrough: false,
        })
        .chain(links.iter().map(|link| koru_draw::text::RangeStyle {
            range: link.range.clone(),
            color: Some(theme::p().accent),
            strikethrough: false,
        }))
        .collect();
    engine.styled_layout_in(
        display,
        style.metrics,
        theme::chat_stack(),
        style.weight.value(),
        wrap_width,
        None,
        &ranges,
    )
}

fn reusable(message: &MessageLayout, input: &MessageInput) -> bool {
    message.id == Some(input.id)
        && message.role == input.role
        && message.state == input.state
        && message.markdown == input.markdown
        && message.display == input.display
        && (message.wrap_width - input.wrap_width).abs() <= 0.5
        && message.usage_text == input.usage_text
        && message
            .sources
            .as_ref()
            .map(|sources| sources.sources().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
            == input.sources
        && message.attachments == input.attachments
        && message.blocks.len() == input.blocks.len()
        && message
            .blocks
            .iter()
            .zip(&input.blocks)
            .all(|(old, (id, markdown))| {
                old.id == *id
                    && old.markdown == *markdown
                    && (old.wrap_width - input.wrap_width).abs() <= 0.5
            })
}

#[cfg(test)]
mod tests;
