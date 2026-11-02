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
    /// The pictures and files sent with a message the person wrote, drawn
    /// as tiles under its text at sizes known before any thumbnail loads.
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

impl BlockLayout {
    pub fn height(&self) -> f32 {
        if self.image.is_some() {
            return 260.0;
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

    /// The link under `(x, y)`, measured from the block's top-left.
    pub fn link_at(&self, x: f32, y: f32) -> Option<&koru_markdown::LinkSpan> {
        if self.table.is_some() || self.image.is_some() {
            return None;
        }
        let byte = byte_at(&self.layout, x, y)?;
        self.links.iter().find(|link| link.range.contains(&byte))
    }
}

/// The byte of `layout`'s text under `(x, y)`, measured from its top-left,
/// when a character is there. parley's exact hit-test still snaps a point
/// above or below the layout to its first or last line, so a pointer
/// anywhere in a column of text would land on every other layout's edge
/// lines at that `x`; the layout's own height is checked first.
pub(crate) fn byte_at(layout: &Layout<Brush>, x: f32, y: f32) -> Option<usize> {
    if x < 0.0 || y < 0.0 || y >= layout.height() {
        return None;
    }
    parley::layout::Cluster::from_point_exact(layout, x, y)
        .map(|(cluster, _)| cluster.text_range().start)
}

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
            engine.simple_layout_wrapped("", theme::role::ui(), input.wrap_width)
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
    if links.is_empty() {
        return engine.simple_layout_wrapped(display, theme::role::ui(), wrap_width);
    }
    let style = theme::role::ui();
    let ranges: Vec<koru_draw::text::RangeStyle> = links
        .iter()
        .map(|link| koru_draw::text::RangeStyle {
            range: link.range.clone(),
            color: Some(theme::p().accent),
            strikethrough: false,
        })
        .collect();
    engine.styled_layout(
        display,
        style.metrics,
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
