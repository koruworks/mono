// SPDX-License-Identifier: MPL-2.0

//! The `koru://` URL, in one place.
//!
//! A Koru link names content and never says where that content lives. That is
//! the same rule the stored reference follows
//! ([ADR-127](../../../docs/decisions/README.md)): a document may move between
//! workspaces after somebody has linked to it, so a link that carried a
//! workspace would go stale the moment it was true.
//!
//! ```text
//! koru://doc/{document-uuid}                whole document
//! koru://doc/{document-uuid}#{node-uuid}    document, scrolled to one block
//! koru://chat/{chat-uuid}#{message-block-uuid} AI chat, optionally at one block
//! koru://skill/{encoded-name}             selected agent skill
//! koru://command/{encoded-name}           selected agent command
//! koru://bot/{bot-uuid}#{encoded-name}    a Bot mentioned in a message to a Bot
//! koru://{node-uuid}                        a bare node reference
//! ```
//!
//! **The block is a fragment, not a path segment.** It is the URI syntax for
//! "same resource, one place inside it", which is exactly what an optional
//! target node is; a path segment would claim the block were a resource of its
//! own. It also keeps the path grammar closed at two segments, so a form this
//! build has never seen — `koru://doc/{id}/history`, say — is unambiguously
//! unknown rather than something a later parser has to disentangle.
//!
//! **An unknown form is refused, never guessed.** What the two callers do with
//! that refusal differs, deliberately: following a link says so out loud,
//! while the Markdown importer keeps the text as text, because failing a whole
//! file over one bad link is the wrong trade.

use uuid::Uuid;

/// The longest link worth looking at. A real one is about sixty bytes; this is
/// checked before anything is allocated, so a hostile string cannot make the
/// parser do work proportional to its length.
const MAX_LEN: usize = 512;

/// Characters that must never reach a parser, whatever they would mean.
///
/// Windows hands a protocol handler the URL through `%1` on a command line, so
/// a quote in it can end an argument and begin another. The registered command
/// quotes its placeholder; this is the other half of that, and it is here
/// rather than in the handler so every caller gets it.
const FORBIDDEN: &[char] = &['"', '<', '>', '|', '&', '^', '\\', '\''];

/// What a `koru://` URL names.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KoruLink {
    /// A document, and optionally one block inside it.
    Document { document: Uuid, node: Option<Uuid> },
    /// An AI chat, and optionally one immutable response block inside it.
    Chat { chat: Uuid, block: Option<Uuid> },
    /// A skill selected from the agent's listing.
    Skill { name: String },
    /// A command selected from the agent's listing.
    Command { name: String },
    /// **A Bot mentioned in a message** written to another Bot: the Bot's
    /// id, which is what identifies it (names are not unique), and the name
    /// it had when it was picked. The name is carried so a reader that no
    /// longer knows the Bot (it was renamed, archived or lives elsewhere)
    /// still shows a name and never an id; it is empty when the link carried
    /// none.
    Bot { bot: Uuid, name: String },
    /// A bare node, with no document named. Written by older exports.
    Node(Uuid),
}

/// Why a string is not a Koru link.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LinkError {
    /// Not a `koru://` URL at all — the ordinary answer for ordinary text.
    NotKoru,
    /// A `koru://` URL in a shape this build does not know. **Never guessed
    /// at**: a link that half-works is worse than one that says it cannot.
    UnknownForm,
    /// The right shape, but an id that is not a UUID.
    BadId,
    /// A decoded command or skill name the agent transport would refuse.
    BadName,
    /// Longer than any real link, or carrying a character that must not travel
    /// on a command line.
    Illegal,
}

/// Read a Koru link.
pub fn parse(text: &str) -> Result<KoruLink, LinkError> {
    if text.len() > MAX_LEN {
        return Err(LinkError::Illegal);
    }
    if text.contains(FORBIDDEN) || text.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(LinkError::Illegal);
    }
    // The scheme is case-insensitive per RFC 3986; the rest of the grammar is
    // matched exactly, because `doc` and `Doc` being the same is a guess.
    let rest = text
        .get(..7)
        .filter(|prefix| prefix.eq_ignore_ascii_case("koru://"))
        .map(|_| &text[7..])
        .ok_or(LinkError::NotKoru)?;

    if rest.contains('%')
        && !rest.starts_with("skill/")
        && !rest.starts_with("command/")
        && !rest.starts_with("bot/")
    {
        return Err(LinkError::Illegal);
    }

    let (path, fragment) = match rest.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment)),
        None => (rest, None),
    };
    let mut segments = path.split('/');
    let first = segments.next().ok_or(LinkError::UnknownForm)?;
    let second = segments.next();
    if segments.next().is_some() {
        // Three segments or more. The grammar is closed at two.
        return Err(LinkError::UnknownForm);
    }

    match (first, second) {
        ("doc", Some(document)) => Ok(KoruLink::Document {
            document: id(document)?,
            node: fragment.map(id).transpose()?,
        }),
        ("chat", Some(chat)) => Ok(KoruLink::Chat {
            chat: id(chat)?,
            block: fragment.map(id).transpose()?,
        }),
        ("skill", Some(name)) if fragment.is_none() => Ok(KoruLink::Skill {
            name: decode_name(name)?,
        }),
        ("command", Some(name)) if fragment.is_none() => Ok(KoruLink::Command {
            name: decode_name(name)?,
        }),
        // The fragment is the Bot's name as it was when picked, not a place
        // inside it: a Bot has no blocks to point at.
        ("bot", Some(bot)) => Ok(KoruLink::Bot {
            bot: id(bot)?,
            name: fragment
                .map(decode_bot_name)
                .transpose()?
                .unwrap_or_default(),
        }),
        // A bare node takes no fragment: there is nothing for it to be inside.
        (node, None) if fragment.is_none() && !node.is_empty() => Ok(KoruLink::Node(id(node)?)),
        _ => Err(LinkError::UnknownForm),
    }
}

/// Write a selected skill's durable link spelling.
pub fn skill_link(name: &str) -> Result<String, LinkError> {
    Ok(format!("koru://skill/{}", encode_name(name)?))
}

/// Write a selected command's durable link spelling.
pub fn command_link(name: &str) -> Result<String, LinkError> {
    Ok(format!("koru://command/{}", encode_name(name)?))
}

/// The most characters of a Bot's name a mention link carries.
pub const BOT_NAME_CHARS: usize = 64;

/// The most bytes the encoded name may take, so the whole link stays well
/// inside [`MAX_LEN`] whatever script the name is written in.
const BOT_NAME_ENCODED: usize = 384;

/// Write a mention of a Bot: its id, and the name it is shown by.
///
/// The name is cleaned first, so the link is always one that [`parse`] reads
/// back: every run of whitespace or control characters becomes one space, the
/// ends are trimmed, and a name too long for a link is cut at a character.
/// A name with nothing left writes the bare `koru://bot/{id}`.
pub fn bot_link(bot: Uuid, name: &str) -> String {
    let mut encoded = String::new();
    for (count, character) in bot_name(name).chars().enumerate() {
        let mut piece = String::new();
        let mut buffer = [0u8; 4];
        for byte in character.encode_utf8(&mut buffer).bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                piece.push(char::from(byte));
            } else {
                piece.push('%');
                piece.push_str(&format!("{byte:02X}"));
            }
        }
        if count >= BOT_NAME_CHARS || encoded.len() + piece.len() > BOT_NAME_ENCODED {
            break;
        }
        encoded.push_str(&piece);
    }
    // A cut can leave an encoded space at the end; a name never ends in one.
    while let Some(trimmed) = encoded.strip_suffix("%20") {
        encoded.truncate(trimmed.len());
    }
    if encoded.is_empty() {
        format!("koru://bot/{bot}")
    } else {
        format!("koru://bot/{bot}#{encoded}")
    }
}

/// [`bot_link`] for an id held as text, the way a composer's chip holds it.
/// `None` when the text is not an id: nothing is written for a Bot that
/// cannot be named exactly.
pub fn bot_link_for(bot: &str, name: &str) -> Option<String> {
    Uuid::parse_str(bot).ok().map(|bot| bot_link(bot, name))
}

/// A Bot's name as a mention shows it: one line, no control characters,
/// single spaces, trimmed.
pub fn bot_name(name: &str) -> String {
    name.split(|character: char| character.is_whitespace() || character.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_bot_name(encoded: &str) -> Result<String, LinkError> {
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut source = encoded.bytes();
    while let Some(byte) = source.next() {
        match byte {
            b'%' => {
                let hi = source.next().and_then(|b| (b as char).to_digit(16));
                let lo = source.next().and_then(|b| (b as char).to_digit(16));
                let (Some(hi), Some(lo)) = (hi, lo) else {
                    return Err(LinkError::BadName);
                };
                bytes.push(((hi << 4) | lo) as u8);
            }
            b if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') => {
                bytes.push(b);
            }
            _ => return Err(LinkError::BadName),
        }
    }
    let name = String::from_utf8(bytes).map_err(|_| LinkError::BadName)?;
    // Only a name this writer could have written is read back: anything
    // else is a link somebody made by hand, and stays text.
    if name.chars().count() > BOT_NAME_CHARS || bot_name(&name) != name {
        return Err(LinkError::BadName);
    }
    Ok(name)
}

fn encode_name(name: &str) -> Result<String, LinkError> {
    if !koru_session_protocol::valid_command_name(name) {
        return Err(LinkError::BadName);
    }
    let mut encoded = String::new();
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    Ok(encoded)
}

fn decode_name(encoded: &str) -> Result<String, LinkError> {
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut source = encoded.bytes();
    while let Some(byte) = source.next() {
        match byte {
            b'%' => {
                let hi = source.next().and_then(|b| (b as char).to_digit(16));
                let lo = source.next().and_then(|b| (b as char).to_digit(16));
                let (Some(hi), Some(lo)) = (hi, lo) else {
                    return Err(LinkError::BadName);
                };
                bytes.push(((hi << 4) | lo) as u8);
            }
            b if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') => {
                bytes.push(b);
            }
            _ => return Err(LinkError::BadName),
        }
    }
    let name = String::from_utf8(bytes).map_err(|_| LinkError::BadName)?;
    koru_session_protocol::valid_command_name(&name)
        .then_some(name)
        .ok_or(LinkError::BadName)
}

/// Write a link to a document, and optionally one block inside it.
pub fn document_link(document: Uuid, node: Option<Uuid>) -> String {
    match node {
        Some(node) => format!("koru://doc/{document}#{node}"),
        None => format!("koru://doc/{document}"),
    }
}

/// Write a link to an AI chat, and optionally one immutable message block.
pub fn chat_link(chat: Uuid, block: Option<Uuid>) -> String {
    match block {
        Some(block) => format!("koru://chat/{chat}#{block}"),
        None => format!("koru://chat/{chat}"),
    }
}

/// Write a bare node reference.
pub fn node_link(node: Uuid) -> String {
    format!("koru://{node}")
}

/// One `[[koru://…]]` found in text: the byte range of the whole token,
/// brackets included, and the link it spells.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Wrapped {
    pub range: std::ops::Range<usize>,
    pub link: KoruLink,
}

/// Every **`[[koru://…]]`** in `text`, in order — the spelling the Markdown
/// exporter writes for a pill, and so the spelling a chat message carries a
/// document chip in.
///
/// Read here rather than by each caller so the importer turning the token
/// back into a pill and the chat drawing it as a titled link cannot disagree
/// about what a token is. A `[[…]]` whose inside is not a link this build
/// understands is not a token: it stays text, like any other brackets.
pub fn wrapped(text: &str) -> Vec<Wrapped> {
    scan_wrapped(text, false)
}

fn preceding_slashes(text: &str, open: usize) -> usize {
    text.as_bytes()[..open]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
}

fn scan_wrapped(text: &str, include_escaped: bool) -> Vec<Wrapped> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(open) = text[at..].find("[[") {
        let open = at + open;
        if !include_escaped && preceding_slashes(text, open) % 2 == 1 {
            at = open + 2;
            continue;
        }
        let Some(close) = text[open..].find("]]") else {
            break;
        };
        let close = open + close;
        at = close + 2;
        if let Ok(link) = parse(&text[open + 2..close]) {
            found.push(Wrapped {
                range: open..close + 2,
                link,
            });
        }
    }
    found
}

/// Escape a typed link-shaped token so a later reader cannot mistake words
/// for a selected pill. Call this on ordinary text runs, never on pill output.
pub fn escape_literal_links(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    let mut from = 0;
    for token in scan_wrapped(text, true) {
        let slashes = preceding_slashes(text, token.range.start);
        let prefix_end = token.range.start - slashes;
        escaped.push_str(&text[from..prefix_end]);
        escaped.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        escaped.push_str(&text[token.range.clone()]);
        from = token.range.end;
    }
    escaped.push_str(&text[from..]);
    escaped
}

/// Undo the one escape this codec adds for literal link-shaped words.
pub fn unescape_literal_links(text: &str) -> String {
    let mut unescaped = String::with_capacity(text.len());
    let mut from = 0;
    for token in scan_wrapped(text, true) {
        let slashes = preceding_slashes(text, token.range.start);
        if slashes % 2 == 0 {
            continue;
        }
        let prefix_end = token.range.start - slashes;
        unescaped.push_str(&text[from..prefix_end]);
        unescaped.extend(std::iter::repeat_n('\\', (slashes - 1) / 2));
        unescaped.push_str(&text[token.range.clone()]);
        from = token.range.end;
    }
    unescaped.push_str(&text[from..]);
    unescaped
}

fn id(text: &str) -> Result<Uuid, LinkError> {
    Uuid::parse_str(text).map_err(|_| LinkError::BadId)
}

#[cfg(test)]
mod tests;
