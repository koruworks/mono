// SPDX-License-Identifier: MPL-2.0

//! **Search results.** A web search is its query, then one row per result
//! — its title and its site — that opens in the browser through the
//! chat's own `open_web_link`. A search of local files (grep, glob) is its
//! command, then one `path:line  text` row per match.
//!
//! Only what an agent sends reliably is parsed; anything else is drawn as
//! text. What agents send (D17, Step 0):
//! - Claude's web search: `{"query"}` for its input, titled `Search
//!   "query"`; its output one `Title (url)` line per result
//!   (claude-agent-acp's `formatWebSearchHit`), or — from an older CLI —
//!   the model's own dump, whose `Links: [{"title", "url"}, …]` line is
//!   read instead.
//! - Codex's web search: its query as its input (Koru's adapter); the
//!   app-server sends no results.
//! - Claude's Grep and Glob: their title is the command (`grep -n "todo"
//!   src`, ``Find `**/*.rs` ``); the output is the tool's own text —
//!   `path:line:text` rows, or paths, after a `Found 3 files` line.

use koru_i18n::Lang;

use super::{Call, CodeLine, Ink, Piece, Rendered, code_block, input_field, strip_ansi, unfence};

/// One web result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WebHit {
    pub title: String,
    pub url: String,
}

/// One match of a local search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Match {
    pub path: String,
    pub line: Option<u32>,
    pub text: String,
}

pub(super) fn render_web(call: &Call<'_>, lang: Lang, expanded: bool) -> Option<Rendered> {
    let hits = call.output.map(web_hits).unwrap_or_default();
    if hits.is_empty() && call.output.is_some() {
        // Results this module cannot read: the query, then the text.
        let mut rendered = super::text(
            &Call {
                input: None,
                ..call.clone()
            },
            lang,
            expanded,
        );
        if let Some(query) = query(call) {
            rendered.pieces.insert(0, query_header(&query));
        }
        return Some(rendered);
    }
    let mut pieces = Vec::new();
    if let Some(query) = query(call) {
        pieces.push(query_header(&query));
    }
    super::error_block(call, expanded, lang, &mut pieces);
    pieces.extend(hits.into_iter().map(|hit| Piece::Link {
        site: site(&hit.url),
        title: hit.title,
        url: hit.url,
    }));
    Some(Rendered {
        pieces,
        notes: Vec::new(),
    })
}

fn query_header(query: &str) -> Piece {
    Piece::Header {
        text: format!("\u{201C}{query}\u{201D}"),
        mono: false,
    }
}

/// What a web search looked for: its input's `query`, Codex's input
/// itself, or the quoted part of its title.
pub(crate) fn query(call: &Call<'_>) -> Option<String> {
    if let Some(input) = call.input {
        return match serde_json::from_str::<serde_json::Value>(input) {
            Ok(value) => input_field(&value, &["query", "q"]).map(str::to_owned),
            Err(_) => Some(input.to_owned()),
        };
    }
    let label = call.activity.agent_label.as_deref()?.trim();
    let quoted = label.strip_prefix("Search ").unwrap_or(label);
    quoted
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .filter(|query| !query.is_empty())
        .map(str::to_owned)
}

/// The results a web search's output lists: `Title (url)` lines, or a
/// `Links:` line's JSON. Empty when it lists none this can read.
pub(crate) fn web_hits(output: &str) -> Vec<WebHit> {
    let output = unfence(output).1;
    for line in output.lines() {
        if let Some(links) = line.trim().strip_prefix("Links:")
            && let Ok(serde_json::Value::Array(links)) = serde_json::from_str(links.trim())
        {
            return links
                .iter()
                .filter_map(|link| {
                    let url = link.get("url")?.as_str()?;
                    is_web(url).then(|| WebHit {
                        title: link
                            .get("title")
                            .and_then(|title| title.as_str())
                            .unwrap_or(url)
                            .to_owned(),
                        url: url.to_owned(),
                    })
                })
                .collect();
        }
    }
    let lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let hits = lines
        .iter()
        .filter_map(|line| {
            let (title, url) = line.strip_suffix(')')?.rsplit_once(" (")?;
            (is_web(url) && !url.contains(char::is_whitespace)).then(|| WebHit {
                title: title.trim().to_owned(),
                url: url.to_owned(),
            })
        })
        .collect::<Vec<_>>();
    // Every line a result, or the output is not a list of results.
    if hits.len() == lines.len() {
        hits
    } else {
        Vec::new()
    }
}

fn is_web(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// A URL's site, as a person names it: `en.wikipedia.org`, no `www.`.
pub(crate) fn site(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    host.strip_prefix("www.").unwrap_or(host).to_owned()
}

pub(super) fn render_matches(call: &Call<'_>, lang: Lang, expanded: bool) -> Option<Rendered> {
    let output = call.output.map(|output| strip_ansi(unfence(output).1));
    let matches = match output.as_deref() {
        Some(output) => Some(matches(output)?),
        None => None,
    };
    let mut pieces = Vec::new();
    if let Some(label) = call
        .activity
        .agent_label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
    {
        pieces.push(Piece::Header {
            text: label.to_owned(),
            mono: true,
        });
    }
    super::error_block(call, expanded, lang, &mut pieces);
    let rows = matches
        .unwrap_or_default()
        .into_iter()
        .map(|found| {
            let text = match (found.line, found.text.is_empty()) {
                (Some(line), false) => format!("{}:{line}  {}", found.path, found.text.trim()),
                (Some(line), true) => format!("{}:{line}", found.path),
                (None, false) => format!("{}  {}", found.path, found.text.trim()),
                (None, true) => found.path,
            };
            CodeLine::plain(text, Ink::Text)
        })
        .collect::<Vec<_>>();
    if !rows.is_empty() {
        pieces.extend(code_block(rows, expanded, lang));
    }
    Some(Rendered {
        pieces,
        notes: Vec::new(),
    })
}

/// A local search's output as its matches: `path:line:text`, `path:text`
/// or a bare path per line, after an optional `Found N files` line.
/// `None` when a line is none of those, and the output is drawn as text.
pub(crate) fn matches(output: &str) -> Option<Vec<Match>> {
    let mut found = Vec::new();
    for line in output.lines().map(str::trim_end) {
        if line.trim().is_empty()
            || line.starts_with("Found ")
            || line == "No files found"
            || line == "No matches found"
        {
            continue;
        }
        found.push(one_match(line)?);
    }
    Some(found)
}

fn one_match(line: &str) -> Option<Match> {
    // A Windows drive's colon is part of the path.
    let skip = match line.as_bytes() {
        [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic() => 2,
        _ => 0,
    };
    let path_end = line[skip..].find(':').map(|at| at + skip);
    if let Some(end) = path_end {
        let path = &line[..end];
        let rest = &line[end + 1..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && looks_like_path(path) {
            let number = rest[..digits].parse().ok();
            let text = rest[digits..]
                .strip_prefix(':')
                .or_else(|| rest[digits..].strip_prefix('-'))
                .unwrap_or(&rest[digits..]);
            return Some(Match {
                path: path.to_owned(),
                line: number,
                text: text.to_owned(),
            });
        }
    }
    looks_like_path(line).then(|| Match {
        path: line.to_owned(),
        line: None,
        text: String::new(),
    })
}

fn looks_like_path(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with(char::is_whitespace)
        && (text.contains('/') || text.contains('\\') || text.contains('.'))
}
