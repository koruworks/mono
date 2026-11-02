// SPDX-License-Identifier: MPL-2.0

//! **A file read**: its path — and *lines 40–89* when only part was read —
//! then what was read in monospace with its line numbers. No syntax
//! colors: the chat's code blocks have none either.
//!
//! What agents send (D17, Step 0): Claude's read has `{"file_path", "offset",
//! "limit"}` for its input and a title like *Read src/a.rs (40 - 89)*; its
//! output is the file's lines as `N<tab>line`, in a fence long enough to
//! hold any fence inside (claude-agent-acp 0.76.0, `markdownEscape`). An
//! older Claude numbered lines as `   N→line`. Codex reads through commands.

use koru_i18n::{Key, Lang, fill, t};

use super::{Call, CodeLine, Ink, Piece, Rendered, code_block, input_field, strip_ansi, unfence};

pub(super) fn render(call: &Call<'_>, lang: Lang, expanded: bool) -> Option<Rendered> {
    let mut pieces = Vec::new();
    let (path, asked) = target(call);
    let content = call
        .output
        .map(|output| strip_ansi(unfence(output).1))
        .unwrap_or_default();
    let lines = numbered(&content);
    let shown = lines
        .iter()
        .filter_map(|(number, _)| *number)
        .fold(None, |range: Option<(u32, u32)>, n| {
            Some(range.map_or((n, n), |(a, b)| (a.min(n), b.max(n))))
        });
    // The lines read, as the output numbers them; else as the call asked.
    let range = shown.filter(|(first, _)| *first > 1).or(asked);
    if let Some(path) = path {
        let text = match range {
            Some((first, last)) => format!(
                "{path} \u{00B7} {}",
                fill(
                    t(lang, Key::AiToolLines),
                    &[&first.to_string(), &last.to_string()]
                )
            ),
            None => path,
        };
        pieces.push(Piece::Header { text, mono: false });
    }
    super::error_block(call, expanded, lang, &mut pieces);
    if !lines.is_empty() {
        pieces.extend(code_block(
            lines
                .into_iter()
                .map(|(number, text)| CodeLine {
                    gutter: number.map(|n| n.to_string()).unwrap_or_default(),
                    ink: if number.is_some() {
                        Ink::Text
                    } else {
                        Ink::Dim
                    },
                    text,
                })
                .collect(),
            expanded,
            lang,
        ));
    }
    Some(Rendered {
        pieces,
        notes: Vec::new(),
    })
}

/// The file a read names, and the lines it asked for: from its input,
/// else from its title (*Read src/a.rs (40 - 89)*).
fn target(call: &Call<'_>) -> (Option<String>, Option<(u32, u32)>) {
    let input = call
        .input
        .and_then(|input| serde_json::from_str::<serde_json::Value>(input).ok());
    if let Some(input) = &input
        && let Some(path) = input_field(input, &["file_path", "path", "absolute_path"])
    {
        let number = |key: &str| input.get(key).and_then(serde_json::Value::as_u64);
        let range = match (number("offset"), number("limit")) {
            (Some(offset), Some(limit)) if limit > 0 => {
                Some((offset.max(1) as u32, (offset.max(1) + limit - 1) as u32))
            }
            _ => None,
        };
        return (Some(path.to_owned()), range);
    }
    let Some(label) = call.activity.agent_label.as_deref() else {
        return (None, None);
    };
    let Some(rest) = label.trim().strip_prefix("Read ") else {
        return (None, None);
    };
    // `path (a - b)` or `path (from line n)`.
    if let Some((path, range)) = rest.rsplit_once(" (")
        && let Some(range) = range.strip_suffix(')')
    {
        let range = range.split_once(" - ").and_then(|(a, b)| {
            Some((a.trim().parse::<u32>().ok()?, b.trim().parse::<u32>().ok()?))
        });
        return (Some(path.to_owned()), range);
    }
    let path = rest.trim();
    ((path != "File").then(|| path.to_owned()), None)
}

/// A read's lines, each with the number the agent gave it — `N<tab>line`
/// or `   N→line` — when every numbered-looking line has one; a line
/// without (*[File truncated …]*) keeps none.
pub(crate) fn numbered(content: &str) -> Vec<(Option<u32>, String)> {
    let mut lines = content
        .lines()
        .map(|line| match split_number(line) {
            Some((number, text)) => (Some(number), text.to_owned()),
            None => (None, line.to_owned()),
        })
        .collect::<Vec<_>>();
    while lines
        .last()
        .is_some_and(|(n, text)| n.is_none() && text.trim().is_empty())
    {
        lines.pop();
    }
    // Numbers only when most lines carry one: a file whose lines happen to
    // start with digits and a tab is not numbered by that.
    let numbered = lines.iter().filter(|(n, _)| n.is_some()).count();
    if numbered * 2 < lines.len() {
        return content
            .lines()
            .map(|line| (None, line.to_owned()))
            .collect();
    }
    lines
}

fn split_number(line: &str) -> Option<(u32, &str)> {
    let trimmed = line.trim_start();
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let number = trimmed[..digits].parse().ok()?;
    let rest = &trimmed[digits..];
    rest.strip_prefix('\t')
        .or_else(|| rest.strip_prefix('\u{2192}'))
        .map(|text| (number, text))
}
