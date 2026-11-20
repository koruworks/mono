// SPDX-License-Identifier: MPL-2.0

//! Render selected agent pills for a provider without losing their identity
//! in the stored chat message.
//!
//! ## A Bot mentioned in a message
//!
//! The stored message keeps each Bot link. Ordinary prompt rendering shows
//! its display name as `@Name`; the native engine separately reads structured
//! recipient identities through [`mentioned_bots`].

use koru_link::KoruLink;

#[cfg(test)]
mod mention_tests;

/// A link's display name bound (`koru_link::BOT_NAME_CHARS`).
const NAME_CHARS: usize = koru_link::BOT_NAME_CHARS;

/// Replace selected skill/command links at their exact positions in a stored
/// message. Literal link-shaped words were escaped by the composer and remain
/// words. Document links keep their existing spelling and explicit context.
/// A mentioned Bot reads as `@Name` and asks for nothing.
pub fn render_prompt_text(markdown: &str) -> String {
    render(markdown)
}

/// Structured Bot mentions outside code. The engine uses these identities for
/// recipient fan-out; display names and ordinary typed `@words` confer no address.
pub fn mentioned_bots(markdown: &str) -> Vec<uuid::Uuid> {
    let code = code_ranges(markdown);
    let mut result = Vec::new();
    for token in koru_link::wrapped(markdown) {
        if let KoruLink::Bot { bot, .. } = token.link
            && !code
                .iter()
                .any(|r| r.start <= token.range.start && token.range.end <= r.end)
            && !result.contains(&bot)
        {
            result.push(bot);
        }
    }
    result
}

fn render(markdown: &str) -> String {
    let mut rendered = String::with_capacity(markdown.len());
    let mut from = 0;
    // Worked out only for a message that mentions a Bot: every other
    // message takes exactly the path it always took.
    let mut code: Option<Vec<std::ops::Range<usize>>> = None;
    for token in koru_link::wrapped(markdown) {
        rendered.push_str(&koru_link::unescape_literal_links(
            &markdown[from..token.range.start],
        ));
        match token.link {
            KoruLink::Skill { name } => {
                rendered.push('$');
                rendered.push_str(&name);
            }
            KoruLink::Command { name } => {
                rendered.push('/');
                rendered.push_str(&name);
            }
            KoruLink::Bot { name, .. } => {
                let code = code.get_or_insert_with(|| code_ranges(markdown));
                if code
                    .iter()
                    .any(|range| range.start <= token.range.start && token.range.end <= range.end)
                {
                    // Inside code the person is showing the text, not
                    // mentioning anybody: it stays as it was written.
                    rendered.push_str(&markdown[token.range.clone()]);
                } else {
                    let name = shown_name(&name);
                    rendered.push('@');
                    rendered.push_str(&name);
                }
            }
            _ => rendered.push_str(&markdown[token.range.clone()]),
        }
        from = token.range.end;
    }
    rendered.push_str(&koru_link::unescape_literal_links(&markdown[from..]));
    rendered
}

/// The name a mention reads as: what the link carried, on one line, with no
/// quotation mark, bounded; `Bot` when
/// the link carried none, never the id.
fn shown_name(name: &str) -> String {
    let name: String = koru_link::bot_name(name)
        .chars()
        .map(|character| if character == '"' { '\'' } else { character })
        .take(NAME_CHARS)
        .collect();
    let name = name.trim();
    if name.is_empty() {
        "Bot".to_owned()
    } else {
        name.to_owned()
    }
}

/// The byte ranges of `markdown` that are code: fenced blocks (three or more
/// backticks or tildes opening a line, to the matching fence or the end) and
/// inline spans (a run of backticks to the next run of the same length, on
/// the same line).
fn code_ranges(markdown: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut fence: Option<(u8, usize, usize)> = None;
    let mut at = 0;
    for line in markdown.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let trimmed = line.trim_start_matches([' ', '\t']);
        let marker = trimmed.bytes().next().filter(|b| matches!(b, b'`' | b'~'));
        let run = marker.map_or(0, |marker| {
            trimmed.bytes().take_while(|byte| *byte == marker).count()
        });
        match fence {
            Some((open, length, begun)) => {
                if marker == Some(open) && run >= length && trimmed[run..].trim().is_empty() {
                    ranges.push(begun..at);
                    fence = None;
                }
            }
            None if run >= 3 => fence = Some((marker.unwrap_or(b'`'), run, start)),
            None => inline_code(line, start, &mut ranges),
        }
    }
    if let Some((_, _, begun)) = fence {
        ranges.push(begun..markdown.len());
    }
    ranges
}

/// The inline code spans of one line, as byte ranges of the whole message.
fn inline_code(line: &str, offset: usize, ranges: &mut Vec<std::ops::Range<usize>>) {
    let bytes = line.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'`' {
            at += 1;
            continue;
        }
        let open = at;
        while at < bytes.len() && bytes[at] == b'`' {
            at += 1;
        }
        let length = at - open;
        let mut seek = at;
        let mut close = None;
        while seek < bytes.len() {
            if bytes[seek] != b'`' {
                seek += 1;
                continue;
            }
            let begun = seek;
            while seek < bytes.len() && bytes[seek] == b'`' {
                seek += 1;
            }
            if seek - begun == length {
                close = Some(seek);
                break;
            }
        }
        match close {
            Some(end) => {
                ranges.push(offset + open..offset + end);
                at = end;
            }
            // An opening run nothing closes is ordinary text.
            None => {}
        }
    }
}

#[cfg(test)]
mod tests;
