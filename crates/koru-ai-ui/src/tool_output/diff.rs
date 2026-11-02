// SPDX-License-Identifier: MPL-2.0

//! **A change to files**: each file's path, then its lines — added in
//! green, removed in red, the rest as context — each with its line number.
//!
//! The row keeps a change as a unified diff. Claude's edit arrives as
//! ACP `diff` content (`{path, oldText, newText}`, with the hunk's first
//! line in `locations` once the edit has run), which Koru's ACP adapter
//! writes as a unified diff with `similar`; Codex's `fileChange` carries
//! a diff per file, which its adapter heads with the file's path. A row
//! written before either is `path` over the new text, and reads as that.
//! A `` ```diff `` fence in any other output is drawn the same way.

use koru_i18n::Lang;

use super::{Call, CodeLine, Ink, Piece, Rendered, code_block, input_field, unfence};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum DiffKind {
    Context,
    Added,
    Removed,
    /// A hunk's `@@ … @@` line.
    Hunk,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiffLine {
    pub kind: DiffKind,
    /// The line's number in the file it is in: the new file for an added
    /// or a context line, the old one for a removed line. `None` when the
    /// diff did not say where its hunk starts.
    pub number: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiffFile {
    pub path: Option<String>,
    pub lines: Vec<DiffLine>,
}

/// Whether `text` is a unified diff: a hunk header, or a file's
/// `---`/`+++` pair, or git's `diff --git` line.
pub(crate) fn is_diff(text: &str) -> bool {
    let mut previous = "";
    for line in text.lines() {
        if line.starts_with("@@") || line.starts_with("diff --git ") {
            return true;
        }
        if line.starts_with("+++ ") && previous.starts_with("--- ") {
            return true;
        }
        previous = line;
    }
    false
}

/// `text` as files and their lines; `None` when it is not a diff.
pub(crate) fn parse(text: &str) -> Option<Vec<DiffFile>> {
    if !is_diff(text) {
        return None;
    }
    let mut files: Vec<DiffFile> = Vec::new();
    let mut old_path: Option<String> = None;
    let (mut old, mut new): (Option<u32>, Option<u32>) = (None, None);
    let mut in_hunk = false;
    let lines = text.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let next = lines.get(index + 1).copied().unwrap_or_default();
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.push(DiffFile {
                path: rest.split_whitespace().last().map(strip_side),
                lines: Vec::new(),
            });
            in_hunk = false;
            continue;
        }
        if line.starts_with("--- ") && next.starts_with("+++ ") {
            old_path = Some(strip_side(&line[4..]));
            in_hunk = false;
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ ")
            && !in_hunk
        {
            let path = strip_side(path);
            let path = if path == "/dev/null" {
                old_path.take()
            } else {
                Some(path)
            };
            // A `diff --git` line already opened this file.
            match files.last_mut() {
                Some(file) if file.lines.is_empty() => file.path = path.or(file.path.take()),
                _ => files.push(DiffFile {
                    path,
                    lines: Vec::new(),
                }),
            }
            continue;
        }
        if line.starts_with("@@") {
            (old, new) = hunk_starts(line);
            in_hunk = true;
            file(&mut files).lines.push(DiffLine {
                kind: DiffKind::Hunk,
                number: None,
                text: line.to_string(),
            });
            continue;
        }
        if !in_hunk || line.starts_with('\\') {
            // A header line (`index …`, `new file mode …`), or *\ No
            // newline at end of file*.
            if !in_hunk && !line.trim().is_empty() && !is_header(line) {
                file(&mut files).lines.push(DiffLine {
                    kind: DiffKind::Context,
                    number: None,
                    text: line.to_string(),
                });
            }
            continue;
        }
        let (kind, number, text) = match line.as_bytes().first() {
            Some(b'+') => (DiffKind::Added, step(&mut new), &line[1..]),
            Some(b'-') => (DiffKind::Removed, step(&mut old), &line[1..]),
            Some(b' ') => {
                step(&mut old);
                (DiffKind::Context, step(&mut new), &line[1..])
            }
            _ => {
                step(&mut old);
                (DiffKind::Context, step(&mut new), *line)
            }
        };
        file(&mut files).lines.push(DiffLine {
            kind,
            number,
            text: text.to_string(),
        });
    }
    files.retain(|file| !file.lines.is_empty() || file.path.is_some());
    Some(files)
}

fn file(files: &mut Vec<DiffFile>) -> &mut DiffFile {
    if files.is_empty() {
        files.push(DiffFile {
            path: None,
            lines: Vec::new(),
        });
    }
    files.last_mut().unwrap()
}

fn is_header(line: &str) -> bool {
    [
        "index ",
        "new file mode",
        "deleted file mode",
        "old mode",
        "new mode",
        "similarity index",
        "rename from",
        "rename to",
        "Binary files",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

/// `a/src/x.rs` → `src/x.rs`; a path that is only a path stays as it is.
fn strip_side(path: &str) -> String {
    let path = path.trim().split('\t').next().unwrap_or_default();
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
        .to_owned()
}

/// `@@ -12,4 +12,6 @@` → where the hunk's old and new lines start.
fn hunk_starts(line: &str) -> (Option<u32>, Option<u32>) {
    let start = |sign: char| {
        line.split_whitespace()
            .find_map(|part| part.strip_prefix(sign))
            .and_then(|range| range.split(',').next())
            .and_then(|start| start.parse::<u32>().ok())
            // A side that starts at 0 is empty; its first line is 1.
            .map(|start| start.max(1))
    };
    (start('-'), start('+'))
}

fn step(counter: &mut Option<u32>) -> Option<u32> {
    let number = *counter;
    if let Some(value) = counter {
        *value += 1;
    }
    number
}

/// A change: from its unified diff, or — for a row from before Koru kept
/// one — its path over the text it wrote.
pub(super) fn render(call: &Call<'_>, lang: Lang, expanded: bool) -> Option<Rendered> {
    let mut pieces = Vec::new();
    super::error_block(call, expanded, lang, &mut pieces);
    let output = call.output.map(|output| unfence(output).1);
    match output.and_then(parse) {
        Some(files) => {
            let fallback = input_path(call);
            let single = files.len() == 1;
            for file in files {
                let path = file
                    .path
                    .or_else(|| single.then(|| fallback.clone()).flatten());
                pieces.extend(file_pieces(path, &file.lines, expanded, lang));
            }
        }
        None => {
            // `path` over the new text, as rows from before the diff kept.
            let first = output
                .and_then(|output| output.lines().next())
                .map(str::trim);
            let header = input_path(call).or_else(|| {
                first
                    .filter(|line| looks_like_path(line))
                    .map(str::to_owned)
            });
            let skip = usize::from(header.is_some() && header.as_deref() == first);
            if let Some(path) = header {
                pieces.push(Piece::Header {
                    text: path,
                    mono: false,
                });
            }
            if let Some(output) = output {
                let lines = output
                    .lines()
                    .skip(skip)
                    .map(|line| CodeLine::plain(line, Ink::Text))
                    .collect::<Vec<_>>();
                if !lines.is_empty() {
                    pieces.extend(code_block(lines, expanded, lang));
                }
            }
        }
    }
    Some(Rendered {
        pieces,
        notes: Vec::new(),
    })
}

/// One file of a diff: its path as a header, then its lines.
pub(super) fn file_pieces(
    path: Option<String>,
    lines: &[DiffLine],
    expanded: bool,
    lang: Lang,
) -> Vec<Piece> {
    let mut pieces = Vec::new();
    if let Some(path) = path {
        pieces.push(Piece::Header {
            text: path,
            mono: false,
        });
    }
    // A hunk's `@@` line is drawn as a gap between hunks, with the
    // function it is in when the diff names one; the first one is not
    // drawn at all.
    let lines = lines
        .iter()
        .enumerate()
        .filter(|(index, line)| *index > 0 || line.kind != DiffKind::Hunk)
        .map(|(_, line)| code_line(line))
        .collect::<Vec<_>>();
    if !lines.is_empty() {
        pieces.extend(code_block(lines, expanded, lang));
    }
    pieces
}

/// A line that is a file's path and nothing else.
fn looks_like_path(line: &str) -> bool {
    !line.is_empty()
        && !line.contains(char::is_whitespace)
        && (line.contains('/') || line.contains('\\') || line.contains('.'))
}

fn code_line(line: &DiffLine) -> CodeLine {
    let (sign, ink) = match line.kind {
        DiffKind::Added => ("+", Ink::Added),
        DiffKind::Removed => ("-", Ink::Removed),
        DiffKind::Context => (" ", Ink::Text),
        DiffKind::Hunk => {
            let context = line
                .text
                .splitn(3, "@@")
                .nth(2)
                .map(str::trim)
                .unwrap_or_default();
            return CodeLine {
                gutter: String::new(),
                text: format!("\u{22EF} {context}").trim_end().to_owned(),
                ink: Ink::Dim,
            };
        }
    };
    CodeLine {
        gutter: line.number.map(|n| n.to_string()).unwrap_or_default(),
        text: format!("{sign} {}", line.text),
        ink,
    }
}

/// The path a change names in its input: Claude's `file_path`, or the
/// first of Codex's paths.
pub(crate) fn input_path(call: &Call<'_>) -> Option<String> {
    let input = call.input?;
    match serde_json::from_str::<serde_json::Value>(input) {
        Ok(value) => {
            input_field(&value, &["file_path", "path", "notebook_path"]).map(str::to_owned)
        }
        Err(_) => input.lines().next().map(str::trim).map(str::to_owned),
    }
}
