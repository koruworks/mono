// SPDX-License-Identifier: MPL-2.0

//! Read-only Markdown layout and drawing using Koru design tokens.
//!
//! This crate knows no nodes, workspaces, chats, or editing. Product surfaces
//! retain their own behavior and consume the shared presentation result.

use std::borrow::Cow;
use std::ops::Range;

use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Brush};
use parley::Layout;
use parley::layout::{Alignment, AlignmentOptions};
use parley::style::{FontFamily, FontStyle, FontWeight, LineHeight, OverflowWrap, StyleProperty};

pub mod table;

#[derive(Copy, Clone)]
enum StyleRole {
    Ui,
    /// The app's text size in the chat font (`theme::chat_font`), which is
    /// the app font until a person chooses another.
    Chat,
    Document,
}

#[derive(Copy, Clone)]
pub struct Styles {
    role: StyleRole,
    bare_links: bool,
}

impl Styles {
    pub const fn ui() -> Self {
        Self {
            role: StyleRole::Ui,
            bare_links: false,
        }
    }
    /// The AI chat's: the app's size, in the chat font.
    pub const fn chat() -> Self {
        Self {
            role: StyleRole::Chat,
            bare_links: false,
        }
    }
    pub const fn document() -> Self {
        Self {
            role: StyleRole::Document,
            bare_links: false,
        }
    }
    /// A web address written out bare — `https://…` with no `[…](…)` or
    /// `<…>` round it — is drawn and reported as a link too, in a paragraph
    /// and in a table's cell alike ([`bare_links`]). Off by default: a
    /// surface turns it on only when it also follows what it draws as a
    /// link, or the text would look clickable and not be.
    pub const fn with_bare_links(mut self) -> Self {
        self.bare_links = true;
        self
    }
    fn metrics(self) -> theme::TextMetrics {
        match self.role {
            StyleRole::Ui | StyleRole::Chat => theme::role::ui().metrics,
            StyleRole::Document => theme::role::body().metrics,
        }
    }
    fn stack(self) -> &'static str {
        match self.role {
            StyleRole::Ui => theme::ui_stack(),
            StyleRole::Chat => theme::chat_stack(),
            StyleRole::Document => theme::body_font().stack(),
        }
    }
    fn weight(self) -> f32 {
        match self.role {
            StyleRole::Ui => theme::ui_font().weight_for(None),
            StyleRole::Chat => theme::chat_font().weight_for(None),
            StyleRole::Document => theme::body_font().weight(),
        }
    }
}

#[derive(Clone)]
pub struct LinkSpan {
    pub range: Range<usize>,
    pub url: String,
}

/// **The link under a point**, in whichever block drew it — a paragraph's
/// text ([`link_at`]) or one of a table's cells ([`table::link_at`]). Every
/// block answers with this one shape, so a surface follows, lights and
/// describes a link in a cell exactly as it does one in a sentence.
pub struct LinkHit<'a> {
    pub url: &'a str,
    /// Where the link is in `layout`'s text.
    pub range: Range<usize>,
    /// What the link reads as on screen.
    pub title: &'a str,
    /// The layout the link is drawn in, and that layout's top-left in the
    /// space the point was given in.
    pub layout: &'a Layout<Brush>,
    pub origin: (f32, f32),
    /// The table cell, `(row, column)` with the header as row 0, when the
    /// link is in one.
    pub cell: Option<(usize, usize)>,
}

/// The byte of `layout`'s text under `(x, y)`, measured from its top-left,
/// when a character is there. parley's exact hit-test still snaps a point
/// above or below the layout to its first or last line, so a pointer
/// anywhere in a column of text would land on every other layout's edge
/// lines at that `x`; the layout's own height is checked first.
pub fn byte_at(layout: &Layout<Brush>, x: f32, y: f32) -> Option<usize> {
    if x < 0.0 || y < 0.0 || y >= layout.height() {
        return None;
    }
    parley::layout::Cluster::from_point_exact(layout, x, y)
        .map(|(cluster, _)| cluster.text_range().start)
}

/// The link in a block's laid-out `text` under `(x, y)`, measured from the
/// layout's top-left.
pub fn link_at<'a>(
    text: &'a str,
    layout: &'a Layout<Brush>,
    links: &'a [LinkSpan],
    x: f32,
    y: f32,
) -> Option<LinkHit<'a>> {
    let byte = byte_at(layout, x, y)?;
    let link = links.iter().find(|link| link.range.contains(&byte))?;
    Some(LinkHit {
        url: &link.url,
        range: link.range.clone(),
        title: text.get(link.range.clone()).unwrap_or(&link.url),
        layout,
        origin: (0.0, 0.0),
        cell: None,
    })
}

/// **Every web address written out bare in `text`** — `http://` or
/// `https://` and what follows it up to a space — that does not overlap
/// `taken` (the links the Markdown already made, and code). A sentence's
/// closing punctuation and a bracket the address did not open stay text,
/// so `(see https://koru.test/a).` links `https://koru.test/a`.
pub fn bare_links(text: &str, taken: &[Range<usize>]) -> Vec<LinkSpan> {
    let mut found = Vec::new();
    let mut word_start = None;
    for (index, ch) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if !ch.is_whitespace() {
            word_start.get_or_insert(index);
            continue;
        }
        let Some(start) = word_start.take() else {
            continue;
        };
        if let Some(range) = bare_link_in(text, start..index)
            && !taken
                .iter()
                .any(|other| other.start < range.end && range.start < other.end)
        {
            found.push(LinkSpan {
                url: text[range.clone()].to_owned(),
                range,
            });
        }
    }
    found
}

/// The address in one whitespace-free `word` of `text`, if it holds one.
fn bare_link_in(text: &str, word: Range<usize>) -> Option<Range<usize>> {
    let slice = &text[word.clone()];
    let lower = slice.to_ascii_lowercase();
    let at = ["https://", "http://"]
        .iter()
        .filter_map(|scheme| lower.find(scheme))
        .min()?;
    // `xhttps://` is not an address; `(https://` and `<https://` are.
    if slice[..at]
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric)
    {
        return None;
    }
    let start = word.start + at;
    let mut end = word.end;
    loop {
        let candidate = &text[start..end];
        let Some(last) = candidate.chars().next_back() else {
            return None;
        };
        let unopened = |open: char, close: char| {
            last == close && candidate.matches(close).count() > candidate.matches(open).count()
        };
        if matches!(
            last,
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '*' | '_' | '~' | '>' | '`'
        ) || unopened('(', ')')
            || unopened('[', ']')
        {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    let host = text[start..end].split_once("://")?.1;
    host.chars()
        .next()
        .is_some_and(char::is_alphanumeric)
        .then_some(start..end)
}

pub enum Rendered {
    Text {
        text: String,
        layout: Layout<Brush>,
        links: Vec<LinkSpan>,
    },
    Table(table::TableGrid),
    Image {
        url: String,
        alt: String,
    },
    Rule,
}

pub fn render_block(
    engine: &mut TextEngine,
    markdown: &str,
    max_w: f32,
    styles: &Styles,
) -> Rendered {
    let Some(block) = koru_import_markdown::parse(markdown).into_iter().next() else {
        return text_layout(engine, String::new(), max_w, styles, Vec::new(), None);
    };
    render_parsed(engine, block, max_w, styles)
}

/// The same drawing, for a caller that has already parsed.
///
/// A surface showing a whole Markdown document parses it once and lays out
/// only the blocks that are on screen; parsing each block again here would
/// make one document's layout cost a parse per block per resize. The split
/// is where it is so that this and [`render_block`] stay **one** renderer:
/// there is no second place deciding what a heading, a bullet or a quote
/// looks like.
pub fn render_parsed(
    engine: &mut TextEngine,
    block: koru_import_markdown::Block,
    max_w: f32,
    styles: &Styles,
) -> Rendered {
    use koru_import_markdown::{Block, CalloutLine, TextKind};

    let Block::Text(block) = block else {
        return match block {
            Block::Table(source) => {
                table::layout(engine, &source, max_w, table::Sizing::Fit, styles)
                    .map(Rendered::Table)
                    .unwrap_or_else(|_| {
                        text_layout(engine, source, max_w, styles, Vec::new(), None)
                    })
            }
            Block::Image(image) => Rendered::Image {
                url: image.destination,
                alt: image.alt,
            },
            Block::Rule => Rendered::Rule,
            // A box's edges draw as empty lines, which is what sets the box
            // off to the eye; the lines inside carry their place in it, and
            // the first its icon.
            Block::CalloutStart { .. } | Block::CalloutEnd => {
                text_layout(engine, String::new(), max_w, styles, Vec::new(), None)
            }
            Block::Text(_) => unreachable!(),
        };
    };

    let mut prefix = String::new();
    // The first line of a callout box leads with its icon; the lines after
    // it sit in under that.
    match &block.callout {
        Some(CalloutLine::First { icon }) => {
            prefix.push_str(koru_core::callout::icon_of(icon.as_deref()));
            prefix.push(' ');
        }
        Some(CalloutLine::Inside) => prefix.push_str("   "),
        None => {}
    }
    if let Some(list) = block.list {
        prefix.push_str(&"  ".repeat(list.depth as usize));
        prefix.push_str(match block.task {
            Some(true) => "\u{2611} ",
            Some(false) => "\u{2610} ",
            None => "\u{2022} ",
        });
    }
    if matches!(block.kind, TextKind::Quote) {
        prefix.push_str("\u{2502} ");
    }
    let prefix_len = prefix.len();
    let text = prefix + &block.text;
    let mut links = block
        .links
        .iter()
        .map(|link| LinkSpan {
            range: prefix_len + link.start..prefix_len + link.end,
            url: link.url.clone(),
        })
        .collect::<Vec<_>>();
    if styles.bare_links && !matches!(block.kind, TextKind::Code { .. }) {
        let taken = links
            .iter()
            .map(|link| link.range.clone())
            .chain(
                block
                    .runs
                    .iter()
                    .filter(|run| run.marks.code)
                    .map(|run| prefix_len + run.start..prefix_len + run.end),
            )
            .collect::<Vec<_>>();
        links.extend(bare_links(&text, &taken));
        links.sort_by_key(|link| link.range.start);
    }
    text_layout(
        engine,
        text,
        max_w,
        styles,
        links,
        Some((block, prefix_len)),
    )
}

fn text_layout(
    engine: &mut TextEngine,
    text: String,
    max_w: f32,
    styles: &Styles,
    links: Vec<LinkSpan>,
    parsed: Option<(koru_import_markdown::TextBlock, usize)>,
) -> Rendered {
    use koru_import_markdown::TextKind;
    let (size, line_height, stack, weight) = if let Some((block, _)) = parsed.as_ref() {
        if matches!(block.kind, TextKind::Code { .. }) {
            let style = theme::role::ui();
            (
                style.metrics.size,
                style.metrics.line_height,
                theme::code_stack(),
                theme::code_font().weight_for(None),
            )
        } else if let Some(level) = block.heading.map(|heading| heading.level()) {
            let style = theme::role::section_heading();
            (
                style.metrics.size,
                style.metrics.line_height,
                theme::headings_stack(),
                theme::headings_font().weight_for(Some(level)),
            )
        } else {
            let metrics = styles.metrics();
            (
                metrics.size,
                metrics.line_height,
                styles.stack(),
                styles.weight(),
            )
        }
    } else {
        let metrics = styles.metrics();
        (
            metrics.size,
            metrics.line_height,
            styles.stack(),
            styles.weight(),
        )
    };
    let mut builder = engine
        .layout_cx
        .ranged_builder(&mut engine.font_cx, &text, 1.0, true);
    builder.push_default(StyleProperty::FontFamily(FontFamily::Source(
        Cow::Borrowed(stack),
    )));
    builder.push_default(StyleProperty::FontSize(size));
    builder.push_default(StyleProperty::LineHeight(LineHeight::Absolute(line_height)));
    builder.push_default(StyleProperty::OverflowWrap(OverflowWrap::Anywhere));
    koru_draw::text::push_axes(&mut builder, size, weight);
    builder.push_default(StyleProperty::Brush(Brush(theme::p().text)));
    if let Some((block, prefix_len)) = parsed {
        for run in block.runs {
            let range = prefix_len + run.start..prefix_len + run.end;
            if run.marks.bold {
                builder.push(StyleProperty::FontWeight(FontWeight::BOLD), range.clone());
            }
            if run.marks.italic {
                builder.push(StyleProperty::FontStyle(FontStyle::Italic), range.clone());
            }
            if run.marks.underline {
                builder.push(StyleProperty::Underline(true), range.clone());
            }
            if run.marks.strike {
                builder.push(StyleProperty::Strikethrough(true), range.clone());
            }
            if run.marks.code {
                builder.push(
                    StyleProperty::FontFamily(FontFamily::Source(Cow::Borrowed(
                        theme::code_stack(),
                    ))),
                    range,
                );
            }
        }
    }
    for link in &links {
        builder.push(StyleProperty::Underline(true), link.range.clone());
        builder.push(
            StyleProperty::Brush(Brush(theme::p().accent)),
            link.range.clone(),
        );
    }
    let mut layout = builder.build(&text);
    layout.break_all_lines(Some(max_w));
    layout.align(Alignment::Start, AlignmentOptions::default());
    Rendered::Text {
        text,
        layout,
        links,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_and_link_offsets_follow_display_text() {
        let mut engine = TextEngine::new();
        let Rendered::Text { text, links, .. } = render_block(
            &mut engine,
            "> [Koru](https://koru.test)",
            300.0,
            &Styles::ui(),
        ) else {
            panic!("text")
        };
        assert_eq!(text, "\u{2502} Koru");
        assert_eq!(&text[links[0].range.clone()], "Koru");
    }

    /// A box's first line leads with its icon, the lines inside it sit in
    /// under that, and the box's edges are empty lines.
    #[test]
    fn a_callout_box_reads_as_its_icon_led_first_line_with_its_lines_inside() {
        use koru_import_markdown::{Block, parse};
        let mut engine = TextEngine::new();
        let blocks = parse("> 💡 Keep [this](https://koru.test)\n>\n> inside\n>\n> - item");
        let texts: Vec<_> = blocks
            .into_iter()
            .map(
                |block| match render_parsed(&mut engine, block, 300.0, &Styles::ui()) {
                    Rendered::Text { text, links, .. } => {
                        let linked = links
                            .first()
                            .map(|link| text[link.range.clone()].to_owned());
                        (text, linked)
                    }
                    _ => panic!("text"),
                },
            )
            .collect();
        assert_eq!(
            texts,
            vec![
                (String::new(), None),
                ("💡 Keep this".to_owned(), Some("this".to_owned())),
                ("   inside".to_owned(), None),
                ("   \u{2022} item".to_owned(), None),
                (String::new(), None),
            ]
        );
        assert!(matches!(
            render_parsed(&mut engine, Block::CalloutEnd, 300.0, &Styles::ui()),
            Rendered::Text { text, .. } if text.is_empty()
        ));
    }

    fn links_of(markdown: &str, styles: &Styles) -> Vec<(String, String)> {
        let mut engine = TextEngine::new();
        let Rendered::Text { text, links, .. } = render_block(&mut engine, markdown, 400.0, styles)
        else {
            panic!("text")
        };
        links
            .into_iter()
            .map(|link| (text[link.range].to_owned(), link.url))
            .collect()
    }

    /// A link is a link in every kind of text block, and inside bold or
    /// italic; an autolink is one too.
    #[test]
    fn every_text_block_reports_its_links() {
        let link = |text: &str| ("Koru".to_owned(), text.to_owned());
        for markdown in [
            "See [Koru](https://koru.test) here",
            "- an item with [Koru](https://koru.test)",
            "1. a step with [Koru](https://koru.test)",
            "> quoted [Koru](https://koru.test)",
            "## A heading with [Koru](https://koru.test)",
            "**bold [Koru](https://koru.test)**",
            "*[Koru](https://koru.test)*",
        ] {
            assert_eq!(
                links_of(markdown, &Styles::ui()),
                vec![link("https://koru.test")],
                "{markdown}"
            );
        }
        assert_eq!(
            links_of("<https://koru.test/a>", &Styles::ui()),
            vec![(
                "https://koru.test/a".to_owned(),
                "https://koru.test/a".to_owned()
            )]
        );
    }

    /// A bare address is a link only where the surface asked, never in
    /// code, and never a second time inside a link already made.
    #[test]
    fn a_bare_address_is_a_link_when_asked_and_not_in_code() {
        let bare = Styles::ui().with_bare_links();
        let url = "https://koru.test/a?b=1";
        assert!(links_of(&format!("Go to {url}."), &Styles::ui()).is_empty());
        assert_eq!(
            links_of(&format!("Go to {url}."), &bare),
            vec![(url.to_owned(), url.to_owned())]
        );
        assert!(links_of(&format!("Run `curl {url}`"), &bare).is_empty());
        assert_eq!(
            links_of(&format!("[{url}]({url})"), &bare),
            vec![(url.to_owned(), url.to_owned())]
        );
        let mut engine = TextEngine::new();
        let Rendered::Text { links, .. } =
            render_block(&mut engine, &format!("```\n{url}\n```"), 400.0, &bare)
        else {
            panic!("text")
        };
        assert!(links.is_empty());
    }

    #[test]
    fn bare_addresses_leave_punctuation_and_unopened_brackets_as_text() {
        let found = |text: &str| {
            bare_links(text, &[])
                .into_iter()
                .map(|link| link.url)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            found("(see https://koru.test/a). And http://x.test, too!"),
            vec!["https://koru.test/a", "http://x.test"]
        );
        assert_eq!(
            found("https://en.wikipedia.test/wiki/Rust_(language)"),
            vec!["https://en.wikipedia.test/wiki/Rust_(language)"]
        );
        assert!(found("xhttps://koru.test https:// ftp://koru.test").is_empty());
        assert!(bare_links("at https://koru.test", &[3..10]).is_empty());
    }

    #[test]
    fn tables_images_rules_and_invalid_tables_keep_their_meaning() {
        let mut engine = TextEngine::new();
        assert!(matches!(
            render_block(&mut engine, "| A |\n| --- |", 200.0, &Styles::ui()),
            Rendered::Table(_)
        ));
        assert!(matches!(
            render_block(&mut engine, "| A | B |\n| --- |", 200.0, &Styles::ui()),
            Rendered::Text { .. }
        ));
        assert!(matches!(
            render_block(
                &mut engine,
                "![alt](https://example.test/a.png)",
                200.0,
                &Styles::ui()
            ),
            Rendered::Image { .. }
        ));
        assert!(matches!(
            render_block(&mut engine, "---", 200.0, &Styles::ui()),
            Rendered::Rule
        ));
    }
}
