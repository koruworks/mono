// SPDX-License-Identifier: MPL-2.0

//! **A command**: `$ git status` over what it printed, in monospace, on
//! the code background; its exit code on the status line when it is
//! known; a long output folded to its head and tail.
//!
//! What agents send (D17, Step 0): Claude's call has `{"command": …,
//! "description": …}` for its input and its output wrapped in a
//! `` ```console `` fence, and a failed command's words — which begin
//! `Exit code 1` — as its error. Codex's input is the command line itself,
//! its output the command's aggregated output, and a non-zero exit code is
//! put first as `Exit code N` the same way (`codex::normalize`).

use koru_i18n::{Key, Lang, fill, t};

use super::{Call, CodeLine, Ink, Piece, Rendered, code_block, input_field, strip_ansi, unfence};

/// Labels that name a command without saying which: the kind's token, or
/// a title given before the input was written.
const GENERIC: [&str; 4] = ["Terminal", "Bash", "command_execution", "Run command"];

/// The fence info strings a terminal's output comes wrapped in.
const CONSOLE: [&str; 8] = [
    "console",
    "sh",
    "bash",
    "shell",
    "zsh",
    "powershell",
    "text",
    "output",
];

/// The most lines of a command the header shows.
const COMMAND_LINES: usize = 6;

pub(super) fn render(call: &Call<'_>, lang: Lang, expanded: bool) -> Option<Rendered> {
    let mut pieces = Vec::new();
    let mut notes = Vec::new();
    if let Some(command) = command(call) {
        pieces.push(Piece::Header {
            text: header(&command),
            mono: true,
        });
    }
    let mut lines = Vec::new();
    let mut exit = None;
    for (text, ink) in [
        (call.output, Ink::Text),
        (call.error.as_deref(), Ink::Error),
    ] {
        let Some(text) = text else {
            continue;
        };
        let (code, text) = exit_code(&console_text(text));
        exit = exit.or(code);
        lines.extend(
            text.lines()
                .map(|line| CodeLine::plain(line.trim_end(), ink)),
        );
    }
    while lines.last().is_some_and(|line| line.text.is_empty()) {
        lines.pop();
    }
    if !lines.is_empty() {
        pieces.extend(code_block(lines, expanded, lang));
    }
    if let Some(code) = exit {
        notes.push(fill(t(lang, Key::AiToolExitCode), &[&code.to_string()]));
    }
    Some(Rendered { pieces, notes })
}

/// The command a call ran: from its input — a JSON object's `command`
/// (a string, or the words of one), else the input itself when it is
/// not JSON — or else its label when the label is not a generic one.
pub(crate) fn command(call: &Call<'_>) -> Option<String> {
    if let Some(input) = call.input {
        match serde_json::from_str::<serde_json::Value>(input) {
            Ok(value) => {
                if let Some(command) = input_field(&value, &["command", "cmd"]) {
                    return Some(command.to_owned());
                }
                if let Some(words) = value.get("command").and_then(|words| words.as_array()) {
                    let words = words
                        .iter()
                        .filter_map(|word| word.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    if !words.is_empty() {
                        return Some(words);
                    }
                }
            }
            Err(_) => return Some(input.to_owned()),
        }
    }
    call.activity
        .agent_label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty() && !GENERIC.contains(label))
        .map(str::to_owned)
}

/// `$ command`, cut to its first lines.
fn header(command: &str) -> String {
    let mut lines = command.trim().lines();
    let mut text = lines
        .by_ref()
        .take(COMMAND_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    if lines.next().is_some() {
        text.push_str("\n\u{2026}");
    }
    format!("$ {text}")
}

/// A command's output without the fence its agent wrapped it in, nor its
/// terminal escapes.
fn console_text(text: &str) -> String {
    let (info, inner) = unfence(text);
    let inner = match info {
        Some(info) if !CONSOLE.contains(&info.to_ascii_lowercase().as_str()) => text,
        _ => inner,
    };
    strip_ansi(inner)
}

/// A leading `Exit code N` line, as Claude's failed command and Codex's
/// (through Koru's adapter) begin: the code, and the text without it.
pub(crate) fn exit_code(text: &str) -> (Option<i32>, String) {
    let trimmed = text.trim_start();
    let (first, rest) = trimmed.split_once('\n').unwrap_or((trimmed, ""));
    match first
        .trim()
        .strip_prefix("Exit code ")
        .and_then(|code| code.trim().parse::<i32>().ok())
    {
        Some(code) => (Some(code), rest.to_owned()),
        None => (None, text.to_owned()),
    }
}
