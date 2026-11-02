// SPDX-License-Identifier: MPL-2.0

//! Finalizing streamed Markdown into immutable deep-link and search units.
//!
//! There is one cut. `finalize_markdown` turns a finished reply into the
//! blocks it is stored as; `cut_markdown` is the same cut over a reply still
//! arriving, saying of each block whether it is closed — whether any text
//! appended later could still change it. A surface renders a closed block
//! as Markdown while the reply streams, knowing the finished reply cuts it
//! the same way.

use crate::{MessageBlock, MessageBlockId, MessageBlockKind, MessageId};
use pulldown_cmark::{Event, Parser};

pub fn finalize_markdown(message_id: MessageId, markdown: &str) -> Vec<MessageBlock> {
    let lines = texts(&split_lines(markdown));
    spans(&lines)
        .into_iter()
        .enumerate()
        .map(|(ordinal, span)| MessageBlock {
            id: MessageBlockId::new(),
            message_id,
            ordinal: ordinal as u32,
            kind: span.kind,
            markdown: lines[span.start..span.end].join("\n"),
            plain_text: plain_text(&lines, &span),
        })
        .collect()
}

/// One block of Markdown that may still be arriving, cut as
/// `finalize_markdown` cuts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockCut {
    pub kind: MessageBlockKind,
    /// The block as `finalize_markdown` stores it.
    pub markdown: String,
    /// The byte just past the block's last line and its line ending.
    pub end: usize,
    /// Nothing appended to the text can change this block any more: its
    /// last line has ended, a paragraph has a finished line after it (a
    /// blank one, or the start of another block), and a fenced block has
    /// its closing fence.
    pub closed: bool,
}

/// `markdown` cut into the blocks `finalize_markdown` would make of it,
/// each saying whether it is closed. Only the last can be open.
pub fn cut_markdown(markdown: &str) -> Vec<BlockCut> {
    let lines = split_lines(markdown);
    let texts = texts(&lines);
    spans(&texts)
        .into_iter()
        .map(|span| {
            let last = &lines[span.end - 1];
            let closed = last.terminated
                && match span.kind {
                    MessageBlockKind::Code => {
                        span.end > span.start + 1
                            && closes_fence(texts[span.start], texts[span.end - 1])
                    }
                    MessageBlockKind::Paragraph => {
                        lines.get(span.end).is_some_and(|next| next.terminated)
                    }
                    _ => true,
                };
            BlockCut {
                kind: span.kind,
                markdown: texts[span.start..span.end].join("\n"),
                end: last.end,
                closed,
            }
        })
        .collect()
}

/// A line of the text, without its line ending — as `str::lines` gives it.
struct Line<'a> {
    text: &'a str,
    /// The byte just past the line and its ending.
    end: usize,
    /// Whether the line has ended: no text appended later joins it.
    terminated: bool,
}

/// `str::lines`, keeping where each line ends and whether it has.
fn split_lines(markdown: &str) -> Vec<Line<'_>> {
    let mut end = 0;
    markdown
        .split_inclusive('\n')
        .map(|piece| {
            end += piece.len();
            match piece.strip_suffix('\n') {
                Some(line) => Line {
                    text: line.strip_suffix('\r').unwrap_or(line),
                    end,
                    terminated: true,
                },
                None => Line {
                    text: piece,
                    end,
                    terminated: false,
                },
            }
        })
        .collect()
}

fn texts<'a>(lines: &[Line<'a>]) -> Vec<&'a str> {
    lines.iter().map(|line| line.text).collect()
}

/// A block, as the lines `start..end` it spans.
struct Span {
    kind: MessageBlockKind,
    start: usize,
    end: usize,
}

fn spans(lines: &[&str]) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        let (kind, end) = if is_fence(line) {
            (MessageBlockKind::Code, fence_end(lines, index))
        } else if heading_text(line).is_some() {
            (MessageBlockKind::Heading, index + 1)
        } else if list_text(line).is_some() {
            (MessageBlockKind::ListItem, index + 1)
        } else if line.starts_with('>') {
            (MessageBlockKind::Quote, index + 1)
        } else if is_rule(line) {
            (MessageBlockKind::Rule, index + 1)
        } else {
            (MessageBlockKind::Paragraph, paragraph_end(lines, index))
        };
        spans.push(Span {
            kind,
            start: index,
            end,
        });
        index = end;
    }
    spans
}

fn plain_text(lines: &[&str], span: &Span) -> String {
    let first = lines[span.start];
    match span.kind {
        MessageBlockKind::Code => {
            let plain_end = if span.end > span.start + 1 && is_fence(lines[span.end - 1]) {
                span.end - 1
            } else {
                span.end
            };
            lines[span.start + 1..plain_end].join("\n")
        }
        MessageBlockKind::Heading => inline_plain_text(heading_text(first).unwrap_or_default()),
        MessageBlockKind::ListItem => inline_plain_text(list_text(first).unwrap_or_default()),
        MessageBlockKind::Quote => {
            inline_plain_text(first.strip_prefix('>').unwrap_or(first).trim_start())
        }
        MessageBlockKind::Rule => String::new(),
        MessageBlockKind::Paragraph | MessageBlockKind::Other => {
            inline_plain_text(&lines[span.start..span.end].join("\n"))
        }
    }
}

fn paragraph_end(lines: &[&str], start: usize) -> usize {
    let mut end = start + 1;
    while end < lines.len() && !lines[end].trim().is_empty() && !is_structural_start(lines[end]) {
        end += 1;
    }
    end
}

fn inline_plain_text(markdown: &str) -> String {
    let mut plain = String::new();
    for event in Parser::new(markdown) {
        match event {
            Event::Text(text) | Event::Code(text) => plain.push_str(&text),
            Event::SoftBreak | Event::HardBreak => plain.push(' '),
            _ => {}
        }
    }
    plain
}

/// Where a fenced block opened at `start` ends: past its closing fence, or
/// at the end of the text when it has none yet.
fn fence_end(lines: &[&str], start: usize) -> usize {
    let mut end = start + 1;
    while end < lines.len() {
        end += 1;
        if closes_fence(lines[start], lines[end - 1]) {
            break;
        }
    }
    end
}

/// Whether `line` closes the fence `opening` opened.
fn closes_fence(opening: &str, line: &str) -> bool {
    let fence_char = opening.trim_start().chars().next().unwrap_or('`');
    line.trim_start()
        .starts_with(&fence_char.to_string().repeat(3))
}

fn is_structural_start(line: &str) -> bool {
    is_fence(line)
        || heading_text(line).is_some()
        || list_text(line).is_some()
        || line.starts_with('>')
        || is_rule(line)
}

fn is_fence(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("```") || line.starts_with("~~~")
}

fn heading_text(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let hashes = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    (1..=6)
        .contains(&hashes)
        .then(|| line[hashes..].strip_prefix(' '))
        .flatten()
}

fn list_text(line: &str) -> Option<&str> {
    let line = line.trim_start();
    for prefix in ["- ", "* ", "+ "] {
        if let Some(text) = line.strip_prefix(prefix) {
            return Some(text);
        }
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        line[digits..].strip_prefix(". ")
    } else {
        None
    }
}

fn is_rule(line: &str) -> bool {
    matches!(line.trim(), "---" | "***" | "___")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalization_preserves_markdown_and_builds_search_text() {
        let message = MessageId::new();
        let markdown = "# **Heading**\n\nTwo *lines*\nin a [paragraph](https://example.com).\n\n- `item`\n\n```rust\nlet x = 1;\n```";
        let blocks = finalize_markdown(message, markdown);
        assert_eq!(
            blocks.iter().map(|block| block.kind).collect::<Vec<_>>(),
            vec![
                MessageBlockKind::Heading,
                MessageBlockKind::Paragraph,
                MessageBlockKind::ListItem,
                MessageBlockKind::Code,
            ]
        );
        assert_eq!(blocks[0].plain_text, "Heading");
        assert_eq!(blocks[1].plain_text, "Two lines in a paragraph.");
        assert_eq!(blocks[2].plain_text, "item");
        assert_eq!(blocks[3].plain_text, "let x = 1;");
        assert_eq!(blocks[3].markdown, "```rust\nlet x = 1;\n```");
        assert!(blocks.iter().all(|block| block.message_id == message));
        assert_eq!(
            blocks.iter().map(|block| block.ordinal).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn empty_markdown_has_no_finalized_blocks() {
        assert!(finalize_markdown(MessageId::new(), "\n\n").is_empty());
    }

    #[test]
    fn an_unclosed_fence_runs_to_the_end_and_keeps_its_text() {
        let blocks = finalize_markdown(MessageId::new(), "```\nlet x = 1;\nlet y = 2;");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, MessageBlockKind::Code);
        assert_eq!(blocks[0].plain_text, "let x = 1;\nlet y = 2;");
    }

    /// The cut of a finished text is the stored cut, block for block.
    #[test]
    fn the_cut_is_the_finalized_cut() {
        let markdown = "# Top US News\r\n\r\n**1. Election eve**\nmore\n> quoted\n- one\n2. two\n---\n~~~\ncode\n~~~\n\ntail";
        let cut = cut_markdown(markdown);
        let blocks = finalize_markdown(MessageId::new(), markdown);
        assert_eq!(
            cut.iter()
                .map(|block| (block.kind, block.markdown.as_str()))
                .collect::<Vec<_>>(),
            blocks
                .iter()
                .map(|block| (block.kind, block.markdown.as_str()))
                .collect::<Vec<_>>(),
        );
        assert_eq!(cut.last().unwrap().end, markdown.len());
    }

    #[test]
    fn a_block_is_closed_once_nothing_appended_can_change_it() {
        let closed = |markdown: &str| {
            cut_markdown(markdown)
                .iter()
                .map(|block| block.closed)
                .collect::<Vec<_>>()
        };
        // A line still being written is open, whatever it is.
        assert_eq!(closed("# Top"), [false]);
        assert_eq!(closed("# Top\n"), [true]);
        assert_eq!(closed("- item\n"), [true]);
        // A paragraph waits for a finished line after it.
        assert_eq!(closed("Para\n"), [false]);
        assert_eq!(closed("Para\n  "), [false]);
        assert_eq!(closed("Para\n\n"), [true]);
        assert_eq!(closed("Para\n---"), [false, false]);
        assert_eq!(closed("Para\n# Next\n"), [true, true]);
        // A fence waits for its closing line.
        assert_eq!(closed("```rust\nlet x = 1;\n"), [false]);
        assert_eq!(closed("```rust\nlet x = 1;\n```"), [false]);
        assert_eq!(closed("```rust\nlet x = 1;\n```\n"), [true]);
        assert_eq!(closed("~~~\n```\n"), [false]);
    }

    /// Cutting the rest of a text after a closed block gives the blocks the
    /// whole text has there — what lets a stream be cut only at its tail.
    #[test]
    fn the_cut_after_a_closed_block_is_the_cut_of_the_whole() {
        let markdown = "# Top\n\nOne\ntwo\n\n```\nx\n```\n- a\n- b\n\nlast";
        let whole = cut_markdown(markdown);
        for (index, block) in whole.iter().enumerate().filter(|(_, block)| block.closed) {
            let rest = cut_markdown(&markdown[block.end..]);
            assert_eq!(
                rest.iter()
                    .map(|cut| (cut.kind, cut.markdown.as_str(), cut.end + block.end))
                    .collect::<Vec<_>>(),
                whole[index + 1..]
                    .iter()
                    .map(|cut| (cut.kind, cut.markdown.as_str(), cut.end))
                    .collect::<Vec<_>>(),
            );
        }
    }
}
