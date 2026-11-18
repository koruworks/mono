// SPDX-License-Identifier: MPL-2.0

//! **Plain-text files: decoded as they are, and HTML with its markup
//! stripped** (TECHNICAL §9, "A plain-text file is added").
//!
//! `.txt`, `.md` and `.csv` are read as they are: Markdown's and CSV's markup
//! is punctuation, which search ignores, and stripping it would be a parser
//! for each to get wrong. HTML is different — tag names, attributes, scripts
//! and styles are words that are not the page's — so it is reduced to its
//! text, with a line break where a block ends.
//!
//! The text is otherwise untouched: line endings become `"\n"`, control
//! characters other than tab and newline are dropped (they are what JSON
//! would escape six bytes wide), and nothing else is normalised.

/// The most of a plain file that becomes its one page, in UTF-8 bytes.
///
/// A plain file is one page, one `Page` message in a `koru-ipc` frame capped
/// at 16 MiB, and one value in a synced document every device carries. Past
/// this the text is cut at a word boundary and the job ends `partial` with
/// `page-cap`, the nearest thing the contract has to say "a cap was reached".
pub const PLAIN_TEXT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// The most of a plain file read from disk. Four times the text cap, room
/// for UTF-16 and for markup that will be stripped.
pub const PLAIN_FILE_MAX_BYTES: u64 = 4 * PLAIN_TEXT_MAX_BYTES as u64;

/// Bytes to text: a byte-order mark if there is one, else UTF-8, else
/// Windows-1252 (the encoding a text file that is not UTF-8 almost always is
/// on the machines this runs on).
pub fn decode(bytes: &[u8]) -> String {
    let text = if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        utf16(rest, u16::from_le_bytes)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        utf16(rest, u16::from_be_bytes)
    } else {
        match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            // A file cut off mid-character by the read limit is still UTF-8.
            Err(error) if error.error_len().is_none() => {
                String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned()
            }
            Err(_) => bytes.iter().map(|&b| windows_1252(b)).collect(),
        }
    };
    tidy(&text)
}

fn utf16(bytes: &[u8], read: fn([u8; 2]) -> u16) -> String {
    let units = bytes.as_chunks::<2>().0.iter().map(|pair| read(*pair));
    char::decode_utf16(units)
        .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// One byte of Windows-1252. 0x80–0x9F are its own; the rest is Latin-1.
fn windows_1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž',
        '\u{8F}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}',
        'ž', 'Ÿ',
    ];
    match byte {
        0x80..=0x9F => HIGH[usize::from(byte - 0x80)],
        _ => char::from(byte),
    }
}

/// Line endings to `"\n"`, and no control characters but tab and newline.
fn tidy(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    text.chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

/// Cut `text` to at most `max` UTF-8 bytes, at the last white space before
/// the limit where there is one. Answers whether anything was cut.
pub fn truncate(mut text: String, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if let Some(space) = text[..end].rfind(char::is_whitespace) {
        end = space;
    }
    text.truncate(end);
    let kept = text.trim_end().len();
    text.truncate(kept);
    (text, true)
}

/// Elements whose content is not the page's text.
const SKIPPED: &[&str] = &[
    "script", "style", "template", "noscript", "svg", "math", "object",
];

/// Elements that end a line.
const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "caption",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "legend",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "title",
    "tr",
    "ul",
];

/// Elements that separate words without ending a line.
const CELLS: &[&str] = &["td", "th", "option"];

/// **HTML to its text.** Not a parser: a forgiving scanner that never fails.
/// Tags are dropped, the content of scripts and styles with them, comments
/// and declarations too; entities are decoded; a block element ends a line;
/// white space inside a line collapses to one space.
pub fn strip_html(html: &str) -> String {
    // A line break in the source is white space, like any other: only the
    // markup decides where a line ends.
    fn text(out: &mut String, source: &str) {
        out.extend(
            source
                .chars()
                .map(|c| if c.is_whitespace() { ' ' } else { c }),
        );
    }
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    while let Some(at) = rest.find(['<', '&']) {
        text(&mut out, &rest[..at]);
        rest = &rest[at..];
        if rest.starts_with('&') {
            let (decoded, used) = entity(rest);
            out.push_str(&decoded);
            rest = &rest[used..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        if let Some(after) = rest.strip_prefix("<![CDATA[") {
            let end = after.find("]]>").unwrap_or(after.len());
            text(&mut out, &after[..end]);
            rest = after.get(end + 3..).unwrap_or("");
            continue;
        }
        let Some((name, closing, after)) = tag(rest) else {
            // A `<` that opens no tag is text.
            out.push('<');
            rest = &rest[1..];
            continue;
        };
        rest = after;
        if name.is_empty() {
            continue; // `<!DOCTYPE …>` or `<?xml …?>`
        }
        if !closing && SKIPPED.contains(&name.as_str()) {
            rest = skip_element(rest, &name);
            continue;
        }
        if BLOCKS.contains(&name.as_str()) {
            out.push('\n');
        } else if CELLS.contains(&name.as_str()) {
            out.push(' ');
        }
    }
    text(&mut out, rest);
    lines(&out)
}

/// The tag at the start of `text` (which starts with `<`): its lowercase
/// name, whether it closes, and what follows its `>`. `None` when `<` opens no
/// tag. A declaration or processing instruction has an empty name.
fn tag(text: &str) -> Option<(String, bool, &str)> {
    let inner = &text[1..];
    let (closing, inner) = match inner.strip_prefix('/') {
        Some(after) => (true, after),
        None => (false, inner),
    };
    let first = inner.chars().next()?;
    let declaration = !closing && (first == '!' || first == '?');
    if !declaration && !first.is_ascii_alphabetic() {
        return None;
    }
    let name: String = if declaration {
        String::new()
    } else {
        inner
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == ':')
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    // Find the closing `>`, stepping over quoted attribute values.
    let mut quote: Option<char> = None;
    for (index, c) in inner.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') if !declaration => quote = Some(c),
            (None, '>') => return Some((name, closing, &inner[index + 1..])),
            _ => {}
        }
    }
    Some((name, closing, ""))
}

/// Skip to just after `</name …>`, case-insensitively; to the end if absent.
fn skip_element<'a>(text: &'a str, name: &str) -> &'a str {
    let lower = text.to_ascii_lowercase();
    let needle = format!("</{name}");
    let mut from = 0;
    while let Some(found) = lower[from..].find(&needle) {
        let at = from + found;
        let next = lower[at + needle.len()..].chars().next();
        if next.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-')) {
            return match text[at..].find('>') {
                Some(end) => &text[at + end + 1..],
                None => "",
            };
        }
        from = at + needle.len();
    }
    ""
}

/// Decode the entity at the start of `text` (which starts with `&`). Answers
/// the text and how many bytes it used; an unknown entity is a literal `&`.
fn entity(text: &str) -> (String, usize) {
    let literal = ("&".to_owned(), 1);
    let Some(end) = text[1..].find(';').map(|i| i + 1).filter(|&end| end <= 33) else {
        return literal;
    };
    let body = &text[1..end];
    let decoded = if let Some(number) = body.strip_prefix('#') {
        let value = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => number.parse::<u32>().ok(),
        };
        match value.and_then(char::from_u32) {
            Some(c) if c != '\0' => c.to_string(),
            Some(_) | None => char::REPLACEMENT_CHARACTER.to_string(),
        }
    } else if let Some(c) = named(body) {
        c.to_string()
    } else {
        return literal;
    };
    (decoded, end + 1)
}

/// The named entities a page is likely to use. An unknown one stays literal.
fn named(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" | "ensp" | "emsp" | "thinsp" => " ",
        "shy" | "zwj" | "zwnj" => "",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "hellip" => "…",
        "mdash" => "—",
        "ndash" => "–",
        "lsquo" => "‘",
        "rsquo" => "’",
        "sbquo" => "‚",
        "ldquo" => "“",
        "rdquo" => "”",
        "bdquo" => "„",
        "laquo" => "«",
        "raquo" => "»",
        "bull" => "•",
        "middot" => "·",
        "euro" => "€",
        "pound" => "£",
        "yen" => "¥",
        "cent" => "¢",
        "sect" => "§",
        "para" => "¶",
        "deg" => "°",
        "times" => "×",
        "divide" => "÷",
        "plusmn" => "±",
        "iexcl" => "¡",
        "iquest" => "¿",
        "Agrave" => "À",
        "Aacute" => "Á",
        "Acirc" => "Â",
        "Atilde" => "Ã",
        "Auml" => "Ä",
        "Aring" => "Å",
        "AElig" => "Æ",
        "Ccedil" => "Ç",
        "Egrave" => "È",
        "Eacute" => "É",
        "Ecirc" => "Ê",
        "Euml" => "Ë",
        "Igrave" => "Ì",
        "Iacute" => "Í",
        "Icirc" => "Î",
        "Iuml" => "Ï",
        "Ntilde" => "Ñ",
        "Ograve" => "Ò",
        "Oacute" => "Ó",
        "Ocirc" => "Ô",
        "Otilde" => "Õ",
        "Ouml" => "Ö",
        "Oslash" => "Ø",
        "Ugrave" => "Ù",
        "Uacute" => "Ú",
        "Ucirc" => "Û",
        "Uuml" => "Ü",
        "Yacute" => "Ý",
        "szlig" => "ß",
        "agrave" => "à",
        "aacute" => "á",
        "acirc" => "â",
        "atilde" => "ã",
        "auml" => "ä",
        "aring" => "å",
        "aelig" => "æ",
        "ccedil" => "ç",
        "egrave" => "è",
        "eacute" => "é",
        "ecirc" => "ê",
        "euml" => "ë",
        "igrave" => "ì",
        "iacute" => "í",
        "icirc" => "î",
        "iuml" => "ï",
        "ntilde" => "ñ",
        "ograve" => "ò",
        "oacute" => "ó",
        "ocirc" => "ô",
        "otilde" => "õ",
        "ouml" => "ö",
        "oslash" => "ø",
        "ugrave" => "ù",
        "uacute" => "ú",
        "ucirc" => "û",
        "uuml" => "ü",
        "yacute" => "ý",
        "yuml" => "ÿ",
        "OElig" => "Œ",
        "oelig" => "œ",
        _ => return None,
    })
}

/// White space inside each line collapsed, empty lines dropped.
fn lines(text: &str) -> String {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests;
