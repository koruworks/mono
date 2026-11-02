// SPDX-License-Identifier: MPL-2.0

use std::ops::Range;

use koru_ai_core::SourceLink;
use koru_draw::{
    text::TextEngine,
    theme::{self, Brush},
};
use parley::Layout;

pub(crate) struct SourceSpan {
    pub source: SourceLink,
    pub bytes: Range<usize>,
}

pub(crate) struct SourcesLayout {
    pub layout: Layout<Brush>,
    pub links: Vec<SourceSpan>,
}

impl SourcesLayout {
    /// The source whose title is under `(x, y)`, measured from the line's
    /// top-left — not the nearest one: the gaps between titles and the room
    /// after the last are not a link.
    pub fn hit(&self, x: f32, y: f32) -> Option<&str> {
        let byte = crate::layout_cache::byte_at(&self.layout, x, y)?;
        self.links
            .iter()
            .find(|link| link.bytes.contains(&byte))
            .map(|link| link.source.url.as_str())
    }

    pub fn sources(&self) -> impl Iterator<Item = &SourceLink> {
        self.links.iter().map(|link| &link.source)
    }
}

pub(crate) fn layout_sources(
    engine: &mut TextEngine,
    sources: &[SourceLink],
    width: f32,
) -> Option<SourcesLayout> {
    if sources.is_empty() {
        return None;
    }
    let mut text = String::from("Sources: ");
    let mut links = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        if index > 0 {
            text.push_str(" · ");
        }
        let start = text.len();
        text.push_str(&source.title);
        let end = text.len();
        links.push(SourceSpan {
            source: source.clone(),
            bytes: start..end,
        });
    }
    let layout = engine.simple_layout_wrapped(&text, theme::role::hint(), width);
    Some(SourcesLayout { layout, links })
}
