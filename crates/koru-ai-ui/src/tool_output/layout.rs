// SPDX-License-Identifier: MPL-2.0

//! A call's [`Piece`]s laid out: each piece one or more layouts, stacked by
//! the work view. Laid out once per change — the surface keeps the result
//! under a fingerprint of the pieces, the width and the theme
//! (`crate::ChatSurface::call_detail`).

use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Brush, role};
use parley::Layout;
use parley::style::{FontFamily, LineHeight, OverflowWrap, StyleProperty};
use vello::peniko::Color;

use super::{CodeLine, Ink, Piece};

/// What a line of a detail does when it is clicked.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum DetailHit {
    /// Show all of a folded block, or fold it again.
    Toggle,
}

/// One laid-out line of a detail.
pub(crate) struct DetailLine {
    pub layout: Rc<Layout<Brush>>,
    /// `None`: the layout's own colors (a block of several inks).
    pub ink: Option<Ink>,
    /// Drawn on the code background.
    pub panel: bool,
    pub hit: Option<DetailHit>,
}

/// The color an ink is drawn in, from the theme now.
pub(crate) fn color(ink: Ink) -> Color {
    let p = theme::p();
    let terminal = theme::terminal_palette();
    match ink {
        Ink::Dim => p.text_dim,
        Ink::Text => p.text,
        Ink::Error => terminal.ansi[1],
        Ink::Link => p.accent,
    }
}

/// A fingerprint of what a detail looks like: its pieces, its width, and
/// the theme its colors were baked in.
pub(crate) fn fingerprint(pieces: &[Piece], width: f32) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    pieces.hash(&mut hasher);
    width.to_bits().hash(&mut hasher);
    for color in [color(Ink::Text), color(Ink::Dim), color(Ink::Error)] {
        color.to_rgba8().to_u32().hash(&mut hasher);
    }
    theme::code_stack().hash(&mut hasher);
    theme::role::meta().metrics.size.to_bits().hash(&mut hasher);
    hasher.finish()
}

/// Lay `pieces` out `width` wide.
pub(crate) fn lay_out(engine: &mut TextEngine, pieces: &[Piece], width: f32) -> Vec<DetailLine> {
    let width = width.max(1.0);
    let mut lines = Vec::new();
    let line = |layout: Layout<Brush>, ink: Option<Ink>| DetailLine {
        layout: Rc::new(layout),
        ink,
        panel: false,
        hit: None,
    };
    for piece in pieces {
        match piece {
            Piece::Meta(text) => lines.push(line(
                engine.simple_layout_wrapped(text, role::meta(), width),
                Some(Ink::Dim),
            )),
            Piece::Markdown(markdown) => {
                for block in koru_ai_core::cut_markdown(markdown) {
                    let code = block.kind == koru_ai_core::MessageBlockKind::Code;
                    let layout = markdown_block(engine, &block.markdown, width);
                    lines.push(DetailLine {
                        panel: code,
                        ..line(layout, Some(Ink::Text))
                    });
                }
            }
            Piece::Code(code) => lines.push(DetailLine {
                panel: true,
                ..line(code_layout(engine, code, width), None)
            }),
            Piece::Toggle(text) => lines.push(DetailLine {
                hit: Some(DetailHit::Toggle),
                ..line(
                    engine.simple_layout_wrapped(text, role::meta(), width),
                    Some(Ink::Link),
                )
            }),
        }
    }
    lines
}

/// One block of Markdown, through the chat's own renderer.
fn markdown_block(engine: &mut TextEngine, markdown: &str, width: f32) -> Layout<Brush> {
    match koru_markdown::render_block(engine, markdown, width, &koru_markdown::Styles::ui()) {
        koru_markdown::Rendered::Text { layout, .. } => layout,
        // A table or a picture in a tool's output reads as its source.
        koru_markdown::Rendered::Table(_) => {
            engine.simple_layout_wrapped_in(markdown, role::meta(), theme::code_stack(), width)
        }
        koru_markdown::Rendered::Image { alt, .. } => {
            engine.simple_layout_wrapped(&alt, role::ui(), width)
        }
        koru_markdown::Rendered::Rule => {
            engine.simple_layout_wrapped(&"\u{2500}".repeat(12), role::ui(), width)
        }
    }
}

/// A block of monospace lines as one layout, each line's gutter dim and
/// its text in its own ink, wrapping at `width`.
fn code_layout(engine: &mut TextEngine, code: &[CodeLine], width: f32) -> Layout<Brush> {
    let gutter = code
        .iter()
        .map(|line| line.gutter.chars().count())
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    let mut ranges = Vec::new();
    for (index, line) in code.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        if gutter > 0 {
            let start = text.len();
            text.push_str(&format!("{:>gutter$}  ", line.gutter));
            ranges.push((start..text.len(), Ink::Dim));
        }
        let start = text.len();
        text.push_str(&line.text);
        ranges.push((start..text.len(), line.ink));
    }
    let style = role::meta();
    let mut builder = engine
        .layout_cx
        .ranged_builder(&mut engine.font_cx, &text, 1.0, true);
    builder.push_default(StyleProperty::FontFamily(FontFamily::Source(
        Cow::Borrowed(theme::code_stack()),
    )));
    builder.push_default(StyleProperty::FontSize(style.metrics.size));
    builder.push_default(StyleProperty::LineHeight(LineHeight::Absolute(
        style.metrics.line_height,
    )));
    builder.push_default(StyleProperty::OverflowWrap(OverflowWrap::Anywhere));
    koru_draw::text::push_axes(
        &mut builder,
        style.metrics.size,
        theme::code_font().weight_for(None),
    );
    builder.push_default(StyleProperty::Brush(Brush(color(Ink::Text))));
    for (range, ink) in ranges {
        if !range.is_empty() && ink != Ink::Text {
            builder.push(StyleProperty::Brush(Brush(color(ink))), range);
        }
    }
    let mut layout = builder.build(&text);
    layout.break_all_lines(Some(width));
    layout.align(
        parley::Alignment::Start,
        parley::AlignmentOptions::default(),
    );
    layout
}
