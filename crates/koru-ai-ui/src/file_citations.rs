// SPDX-License-Identifier: MPL-2.0

//! Display Codex file citations without changing the answer stored in chat.
//! The path is only opened after a person clicks its projected link.

use std::{borrow::Cow, path::PathBuf};

const MARKER: &str = ":codex-file-citation{";
const LINK_PREFIX: &str = "koru-file-citation:";

pub(crate) fn project(markdown: &str, streaming: bool) -> Cow<'_, str> {
    if !markdown.contains(":codex-") && !(streaming && markdown.contains(":cod")) {
        return Cow::Borrowed(markdown);
    }
    let mut result = String::with_capacity(markdown.len());
    let mut rest = markdown;
    let mut fenced = false;
    while !rest.is_empty() {
        let (line, next) = match rest.find('\n') {
            Some(end) => (&rest[..=end], &rest[end + 1..]),
            None => (rest, ""),
        };
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            result.push_str(line);
        } else if fenced {
            result.push_str(line);
        } else {
            project_line(line, streaming, &mut result);
        }
        rest = next;
    }
    Cow::Owned(result)
}

fn project_line(line: &str, streaming: bool, result: &mut String) {
    let mut rest = line;
    let mut code = false;
    while let Some(tick) = rest.find('`') {
        let (before, after) = rest.split_at(tick);
        if code {
            result.push_str(before);
        } else {
            project_prose(before, streaming, result);
        }
        let ticks = after.bytes().take_while(|byte| *byte == b'`').count();
        result.push_str(&after[..ticks]);
        code = !code;
        rest = &after[ticks..];
    }
    if code {
        result.push_str(rest);
    } else {
        project_prose(rest, streaming, result);
    }
}

fn project_prose(line: &str, streaming: bool, result: &mut String) {
    let mut rest = line;
    while let Some(start) = rest.find(MARKER) {
        result.push_str(&rest[..start]);
        let directive = &rest[start + MARKER.len()..];
        let Some(end) = directive_end(directive) else {
            if !streaming {
                result.push_str("[file citation]");
            }
            return;
        };
        let attributes = &directive[..end];
        let path = attribute(attributes, "path");
        let purpose = attribute(attributes, "purpose");
        let label = path
            .as_deref()
            .and_then(|path| path.rsplit(['/', '\\']).next())
            .filter(|name| !name.is_empty())
            .unwrap_or("file");
        let kind = if purpose.as_deref() == Some("output") {
            "File"
        } else {
            "Source"
        };
        let locator = attribute(attributes, "page_number")
            .map(|page| format!(", p. {page}"))
            .or_else(|| attribute(attributes, "sheet").map(|sheet| format!(", {sheet}")))
            .unwrap_or_default();
        let title = format!("{kind}: {label}{locator}");
        if streaming {
            result.push_str(&title);
        } else if let Some(path) = path.as_deref().filter(|path| is_absolute_path(path)) {
            result.push('[');
            result.push_str(&escape_label(&title));
            result.push_str("](");
            result.push_str(&encode_path(&path));
            result.push(')');
        } else if let Some(url) = path
            .as_deref()
            .filter(|path| path.starts_with("https://") || path.starts_with("http://"))
        {
            result.push('[');
            result.push_str(&escape_label(&title));
            result.push_str("](");
            result.push_str(&url);
            result.push(')');
        } else {
            result.push_str(&title);
        }
        rest = &directive[end + 1..];
    }
    if streaming
        && let Some(start) = rest.rfind(":cod")
        && MARKER.starts_with(&rest[start..].trim_end_matches('\n'))
    {
        result.push_str(&rest[..start]);
        return;
    }
    result.push_str(rest);
}

fn attribute(input: &str, name: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
            at += 1;
        }
        if start == at || bytes.get(at) != Some(&b'=') || bytes.get(at + 1) != Some(&b'"') {
            break;
        }
        let key = &input[start..at];
        at += 2;
        let mut value = String::new();
        while at < bytes.len() {
            match bytes[at] {
                b'"' => {
                    at += 1;
                    break;
                }
                b'\\' if bytes.get(at + 1) == Some(&b'"') => {
                    value.push('"');
                    at += 2;
                }
                b'\\' if bytes.get(at + 1) == Some(&b'\\') => {
                    value.push('\\');
                    at += 2;
                }
                _ => {
                    let character = input[at..].chars().next()?;
                    value.push(character);
                    at += character.len_utf8();
                }
            }
        }
        if key == name {
            return Some(value);
        }
    }
    None
}

fn directive_end(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut quoted = false;
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' if quoted && matches!(bytes.get(at + 1), Some(b'"' | b'\\')) => at += 2,
            b'"' => {
                quoted = !quoted;
                at += 1;
            }
            b'}' if !quoted => return Some(at),
            _ => at += 1,
        }
    }
    None
}

fn escape_label(label: &str) -> String {
    label
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn encode_path(path: &str) -> String {
    let mut url = String::from(LINK_PREFIX);
    for byte in path.bytes() {
        use std::fmt::Write;
        let _ = write!(url, "{byte:02x}");
    }
    url
}

pub(crate) fn path_from_link(url: &str) -> Option<PathBuf> {
    let hex = url.strip_prefix(LINK_PREFIX)?;
    if hex.len() % 2 != 0 || hex.len() > 32_768 {
        return None;
    }
    let bytes = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    let path = String::from_utf8(bytes).ok()?;
    is_absolute_path(&path).then(|| PathBuf::from(path))
}

fn is_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    PathBuf::from(path).is_absolute()
        || path.starts_with('/')
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
        || path.starts_with("\\\\")
}

pub(crate) fn address_for_card(url: &str) -> String {
    path_from_link(url)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| url.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_an_inline_windows_source_with_a_clickable_path() {
        let source = "See :codex-file-citation{path=\"C:/Users/Ada/Staff Letter.pdf\" purpose=\"source\"} for details.";
        let shown = project(source, false);
        assert!(shown.starts_with("See [Source: Staff Letter.pdf](koru-file-citation:"));
        let url = shown.split("](").nth(1).unwrap().split(')').next().unwrap();
        assert_eq!(
            path_from_link(url),
            Some(PathBuf::from("C:/Users/Ada/Staff Letter.pdf"))
        );
    }

    #[test]
    fn streaming_hides_incomplete_marker_and_preserves_fenced_examples() {
        assert_eq!(
            project("Answer :codex-file-citation{path=\"C:/partial", true),
            "Answer "
        );
        let example = "```text\n:codex-file-citation{path=\"C:/file\" purpose=\"source\"}\n```";
        assert_eq!(project(example, false), example);
        assert_eq!(project("Answer :codex-file-citati", true), "Answer ");
        let inline = "Here is `:codex-file-citation{path=\"C:/file\"}`.";
        assert_eq!(project(inline, false), inline);
    }

    #[test]
    fn names_output_and_locator_without_opening_a_web_address_as_a_file() {
        let source = ":codex-file-citation{path=\"https://docs.example/x\" purpose=\"output\" page_number=\"4\"}";
        assert_eq!(
            project(source, false),
            "[File: x, p. 4](https://docs.example/x)"
        );
    }

    #[test]
    fn a_brace_and_backslashes_in_a_quoted_path_do_not_end_the_citation() {
        let source =
            r#":codex-file-citation{path="C:\\Files\\{draft}\\letter.txt" purpose="source"}"#;
        let shown = project(source, false);
        assert!(shown.starts_with("[Source: letter.txt](koru-file-citation:"));
        let url = shown.split("](").nth(1).unwrap().split(')').next().unwrap();
        assert_eq!(
            path_from_link(url),
            Some(PathBuf::from(r"C:\Files\{draft}\letter.txt"))
        );
    }

    #[test]
    fn a_posix_path_is_preserved_when_the_chat_moves_between_desktops() {
        let shown = project(
            ":codex-file-citation{path=\"/home/ada/report.txt\" purpose=\"source\"}",
            false,
        );
        let url = shown.split("](").nth(1).unwrap().split(')').next().unwrap();
        assert_eq!(
            path_from_link(url),
            Some(PathBuf::from("/home/ada/report.txt"))
        );
    }
}
