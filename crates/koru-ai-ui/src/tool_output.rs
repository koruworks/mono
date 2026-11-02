// SPDX-License-Identifier: MPL-2.0

//! **What one call did, drawn the way it reads** — the detail a call line
//! in the work view (`crate::work_view`) opens to.
//!
//! A call's row keeps the agent's words as text (`agent_label`,
//! `agent_input`, `agent_output`, `error_message`); this module decides what
//! that text *is* and turns it into [`Piece`]s, pure and testable, which
//! [`layout`] lays out once and the surface keeps until the row changes.
//!
//! **Text** is the fallback every other renderer falls back to: the output
//! as Markdown, so a fenced block is a code block and not three backticks.
//! Whatever the renderer, the tool's own error is shown once, in the error
//! color; an input that says nothing (`{}`) is not shown; and the outcome
//! and the time sit on one quiet line under it all.

use koru_ai_core::{AgentToolKind, CallShape, ToolActivity, ToolActivityStatus};
use koru_i18n::{Key, Lang, fill, t};

use crate::agents::{agent_row_detail, agent_row_outcome};

mod command;
mod diff;
mod file;
pub(crate) mod layout;
mod search;

/// The color a piece of a detail is drawn in, resolved at draw time.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Ink {
    Dim,
    Text,
    Error,
    Added,
    Removed,
    Link,
}

/// One line of a monospace block: an optional gutter (a line number, a
/// diff's sign) in the dim color, then the line in its own.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CodeLine {
    pub gutter: String,
    pub text: String,
    pub ink: Ink,
}

impl CodeLine {
    pub fn plain(text: impl Into<String>, ink: Ink) -> Self {
        Self {
            gutter: String::new(),
            text: text.into(),
            ink,
        }
    }
}

/// What a call's detail is made of, top to bottom.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Piece {
    /// One quiet line: the outcome and the time, a heading, a note.
    Meta(String),
    /// The tool's words as Markdown, laid out block by block as the chat
    /// lays out a reply.
    Markdown(String),
    /// What the call did, on its own line: `$ git status`, a path, a query.
    Header { text: String, mono: bool },
    /// Monospace lines on the code background.
    Code(Vec<CodeLine>),
    /// One result a person can open in the browser.
    Link {
        title: String,
        site: String,
        url: String,
    },
    /// *Show all 120 lines* / *Show fewer lines*.
    Toggle(String),
}

/// A long block shows its first lines and its last, and how many are
/// between; the rest is the person's to open.
pub(crate) const HEAD_LINES: usize = 8;
pub(crate) const TAIL_LINES: usize = 4;

/// Whether `count` lines are folded: only when hiding saves more than a
/// couple of lines.
pub(crate) fn folds(count: usize) -> bool {
    count > HEAD_LINES + TAIL_LINES + 2
}

/// `lines` as a block, folded to its head and tail unless `expanded`, and
/// the toggle that opens or folds it when it is long enough to fold.
pub(crate) fn code_block(lines: Vec<CodeLine>, expanded: bool, lang: Lang) -> Vec<Piece> {
    let count = lines.len();
    if !folds(count) {
        return vec![Piece::Code(lines)];
    }
    if expanded {
        return vec![
            Piece::Code(lines),
            Piece::Toggle(t(lang, Key::AiToolShowFewerLines).to_owned()),
        ];
    }
    let hidden = count - HEAD_LINES - TAIL_LINES;
    let mut shown = lines[..HEAD_LINES].to_vec();
    shown.push(CodeLine::plain(
        format!(
            "\u{22EF} {}",
            fill(t(lang, Key::AiToolMoreLines), &[&hidden.to_string()])
        ),
        Ink::Dim,
    ));
    shown.extend_from_slice(&lines[count - TAIL_LINES..]);
    vec![
        Piece::Code(shown),
        Piece::Toggle(fill(
            t(lang, Key::AiToolShowAllLines),
            &[&count.to_string()],
        )),
    ]
}

/// The detail of an agent's call: what it did, its error once, what it
/// produced, and its outcome and time.
pub(crate) fn pieces(
    activity: &ToolActivity,
    lang: Lang,
    now_ms: i64,
    expanded: bool,
) -> Vec<Piece> {
    let call = Call::of(activity);
    let rendered = match Shape::of(activity) {
        Shape::Command => command::render(&call, lang, expanded),
        Shape::Edit => diff::render(&call, lang, expanded),
        Shape::Read => file::render(&call, lang, expanded),
        Shape::Web => search::render_web(&call, lang, expanded),
        Shape::Matches => search::render_matches(&call, lang, expanded),
        Shape::Other => None,
    }
    .unwrap_or_else(|| text(&call, lang, expanded));
    let mut pieces = rendered.pieces;
    pieces.push(Piece::Meta(status_line(
        activity,
        call.error.is_none() && rendered.notes.is_empty(),
        &rendered.notes,
        lang,
        now_ms,
    )));
    pieces
}

/// What a call is, for drawing it: by the kind it was filed under, and
/// by its label's shape when the kind says only *a tool*.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    Command,
    Edit,
    Read,
    /// A web search: its query and its results.
    Web,
    /// A search of local files: grep, glob.
    Matches,
    Other,
}

impl Shape {
    pub fn of(activity: &ToolActivity) -> Self {
        match AgentToolKind::parse(&activity.tool_name) {
            Some(AgentToolKind::Command) => Self::Command,
            Some(AgentToolKind::FileChange) => Self::Edit,
            Some(AgentToolKind::Read) => Self::Read,
            kind => match (
                kind,
                activity.agent_label.as_deref().and_then(CallShape::of),
            ) {
                (_, Some(CallShape::Read)) => Self::Read,
                (_, Some(CallShape::Search)) => Self::Matches,
                // *Fetch https://…* reads a page: its answer is text.
                (_, Some(CallShape::Fetch)) => {
                    if activity
                        .agent_label
                        .as_deref()
                        .is_some_and(|label| label.trim_start().starts_with("Fetch"))
                    {
                        Self::Other
                    } else {
                        Self::Web
                    }
                }
                (Some(AgentToolKind::Search), _) => Self::Web,
                _ => Self::Other,
            },
        }
    }
}

/// What a renderer made of a call: its pieces, and short notes for its
/// status line (*Exit code 1*).
#[derive(Default)]
pub(crate) struct Rendered {
    pub pieces: Vec<Piece>,
    pub notes: Vec<String>,
}

/// A row's words, cleaned once: the error without its fence, the output
/// unless it only repeats the error, the input unless it says nothing.
#[derive(Clone)]
pub(crate) struct Call<'a> {
    pub activity: &'a ToolActivity,
    pub error: Option<String>,
    pub output: Option<&'a str>,
    pub input: Option<&'a str>,
}

impl<'a> Call<'a> {
    pub fn of(activity: &'a ToolActivity) -> Self {
        let error = activity
            .error_message
            .as_deref()
            .map(|message| strip_ansi(unfence(message).1.trim()))
            .filter(|message| !message.is_empty());
        let output = activity
            .agent_output
            .as_deref()
            .map(str::trim)
            .filter(|output| !output.is_empty())
            .filter(|output| !repeats(output, error.as_deref()));
        let input = activity
            .agent_input
            .as_deref()
            .map(str::trim)
            .filter(|input| !says_nothing(input));
        Self {
            activity,
            error,
            output,
            input,
        }
    }
}

/// Whether `output` is only the error again: an agent that reports a
/// failure sends the same words as the failure and as the result.
fn repeats(output: &str, error: Option<&str>) -> bool {
    let Some(error) = error else {
        return false;
    };
    let output = strip_ansi(unfence(output).1.trim());
    output == error || error.contains(output.as_str()) || output.contains(error)
}

/// An input that says nothing: `{}`, `[]`, `null`, `""`.
pub(crate) fn says_nothing(input: &str) -> bool {
    matches!(input.trim(), "" | "{}" | "[]" | "null" | "\"\"")
}

/// The fallback: the input when there is one to show, the error once, and
/// the output as Markdown.
fn text(call: &Call<'_>, lang: Lang, expanded: bool) -> Rendered {
    let mut pieces = Vec::new();
    if let Some(input) = call.input {
        pieces.push(Piece::Meta(t(lang, Key::AiToolInput).to_owned()));
        pieces.extend(code_block(
            json_lines(input)
                .into_iter()
                .map(|line| CodeLine::plain(line, Ink::Text))
                .collect(),
            expanded,
            lang,
        ));
        if call.output.is_some() {
            pieces.push(Piece::Meta(t(lang, Key::AiToolOutput).to_owned()));
        }
    }
    error_block(call, expanded, lang, &mut pieces);
    if let Some(output) = call.output {
        markdown(&strip_ansi(output), expanded, lang, &mut pieces);
    }
    Rendered {
        pieces,
        notes: Vec::new(),
    }
}

/// Markdown as the chat draws it, except that a `diff` block is drawn as
/// a diff: green and red, with line numbers.
fn markdown(text: &str, expanded: bool, lang: Lang, pieces: &mut Vec<Piece>) {
    let mut prose = String::new();
    for block in koru_ai_core::cut_markdown(text) {
        let files = (block.kind == koru_ai_core::MessageBlockKind::Code)
            .then(|| unfence(&block.markdown))
            .filter(|(info, _)| info.is_some_and(|info| info.eq_ignore_ascii_case("diff")))
            .and_then(|(_, inner)| diff::parse(inner));
        let Some(files) = files else {
            if !prose.is_empty() {
                prose.push_str("\n\n");
            }
            prose.push_str(&block.markdown);
            continue;
        };
        if !prose.is_empty() {
            pieces.push(Piece::Markdown(std::mem::take(&mut prose)));
        }
        for file in files {
            pieces.extend(diff::file_pieces(file.path, &file.lines, expanded, lang));
        }
    }
    if !prose.is_empty() {
        pieces.push(Piece::Markdown(prose));
    }
}

/// A JSON input's string field, the first of `keys` it has.
pub(crate) fn input_field<'a>(input: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| input.get(*key)?.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// The tool's own account of its failure, once, in the error color.
pub(crate) fn error_block(call: &Call<'_>, expanded: bool, lang: Lang, pieces: &mut Vec<Piece>) {
    if let Some(error) = &call.error {
        pieces.extend(code_block(
            error
                .lines()
                .map(|line| CodeLine::plain(line, Ink::Error))
                .collect(),
            expanded,
            lang,
        ));
    }
}

/// `text` as JSON pretty-printed when it is JSON, else as its own lines.
pub(crate) fn json_lines(text: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .filter(|value| value.is_object() || value.is_array())
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| text.to_owned())
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The outcome and the time on one line — *Done · 1s · 2 minutes ago* —
/// and, for a failure the tool gave no words for, why, as the host
/// classified it.
pub(crate) fn status_line(
    activity: &ToolActivity,
    reason: bool,
    notes: &[String],
    lang: Lang,
    now_ms: i64,
) -> String {
    let mut parts = vec![if reason {
        agent_row_detail(activity, lang)
    } else {
        agent_row_outcome(activity, lang)
    }];
    parts.extend(notes.iter().cloned());
    if let Some(ended) = activity.completed_at {
        parts.push(crate::tool_groups::seconds(
            ended.millis() - activity.proposed_at.millis(),
        ));
    }
    if activity.status != ToolActivityStatus::ApprovalRequired {
        parts.push(koru_i18n::time::relative_long(
            lang,
            activity.proposed_at.millis(),
            now_ms,
        ));
    }
    parts.join(" \u{00B7} ")
}

/// A text that is one fenced block, without its fences: the info string
/// (`console`, `diff`, …) and the inside. A fence whose end was cut off by
/// the row's bound still loses its opening line. Anything else is returned
/// as it is, with no info string.
pub(crate) fn unfence(text: &str) -> (Option<&str>, &str) {
    let trimmed = text.trim();
    let ticks = trimmed.bytes().take_while(|byte| *byte == b'`').count();
    if ticks < 3 {
        return (None, text);
    }
    let Some((opening, rest)) = trimmed.split_once('\n') else {
        return (None, text);
    };
    let info = opening[ticks..].trim();
    if info.contains('`') {
        return (None, text);
    }
    // The block closes at the first line of nothing but at least as many
    // backticks; if more follows that line, the text is more than one block.
    let rest = rest.trim_end();
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        let bare = line.trim();
        if bare.len() >= ticks && bare.bytes().all(|byte| byte == b'`') {
            if at + line.len() < rest.len() {
                return (None, text);
            }
            let inner = rest[..at].trim_end_matches(['\n', '\r']);
            return ((!info.is_empty()).then_some(info), inner);
        }
        at += line.len();
    }
    ((!info.is_empty()).then_some(info), rest)
}

/// `text` without its terminal escape sequences — colors, cursor moves,
/// titles — which would otherwise draw as `[31m`.
pub(crate) fn strip_ansi(text: &str) -> String {
    if !text.contains('\u{1b}') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte in @..~.
            Some('[') => {
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ESC \.
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests;
