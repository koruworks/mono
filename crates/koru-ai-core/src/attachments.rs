// SPDX-License-Identifier: MPL-2.0

//! Pictures and files a person sends to a local agent with a message
//! (chat-attachments, ADR-479 and ADR-481).
//!
//! An attachment is a copy the chat owns: the AI service stores its bytes
//! beside `ai.db` and keeps one row of metadata. The limits and the
//! allowlist are named here once, so the chat's pre-checks and the
//! service's authority read the same numbers and the same table.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ChatAttachmentId, ChatId, DocumentId, MessageId, Timestamp, WorkspaceId};

/// The most one attachment may weigh: the picture limit ChatGPT, Claude and
/// Grok share. It also bounds what one upload holds in memory.
pub const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;
/// The most attachments one message may carry.
pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 10;
/// The most all of one message's attachments may weigh together.
pub const MAX_MESSAGE_ATTACHMENT_BYTES: u64 = 50 * 1024 * 1024;
/// A picture is previewed and sent natively only within these bounds. One
/// larger, but within [`MAX_ATTACHMENT_BYTES`], is attached as a file.
pub const MAX_PREVIEW_EDGE: u32 = 16_384;
pub const MAX_PREVIEW_PIXELS: u64 = 80_000_000;
/// The long edge of the thumbnail the service writes for every picture.
pub const THUMBNAIL_EDGE: u32 = 512;
/// The copy an agent receives: vendors resize anything above 1568 px on the
/// long edge before the model sees it, and ten 1 MiB copies fit the 16 MiB
/// prompt frame in base64.
pub const SENT_COPY_EDGE: u32 = 1568;
pub const MAX_SENT_COPY_BYTES: u64 = 1024 * 1024;
/// A staged attachment never sent is removed by the start-up sweep after
/// this long; it covers a crash or a window closed with a draft.
pub const STAGED_ATTACHMENT_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// How much of a file the content check reads.
pub const CONTENT_CHECK_BYTES: usize = 64 * 1024;
/// A stored file name is cut to this many characters, keeping its extension.
pub const MAX_ATTACHMENT_NAME_CHARS: usize = 120;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
// Named apart from the session protocol's type of the same name in one
// gateway schema.
#[cfg_attr(feature = "schema", schemars(rename = "ChatAttachmentKind"))]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    /// A picture with a thumbnail and known dimensions.
    Image,
    /// Anything else, including a picture too large to preview.
    File,
    /// A local directory named to an agent without copying its contents.
    Folder,
}

impl AttachmentKind {
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::File => "file",
            Self::Folder => "folder",
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "image" => Some(Self::Image),
            "file" => Some(Self::File),
            "folder" => Some(Self::Folder),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
// Named apart from the session protocol's type of the same name in one
// gateway schema.
#[cfg_attr(feature = "schema", schemars(rename = "ChatAttachmentSource"))]
#[serde(rename_all = "snake_case")]
pub enum AttachmentSource {
    Clipboard,
    Drop,
    FilePicker,
    /// Linked with `@` from a workspace the chat may use, and copied.
    Workspace,
    /// A local path reference with no copied bytes.
    Path,
}

impl AttachmentSource {
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Clipboard => "clipboard",
            Self::Drop => "drop",
            Self::FilePicker => "file_picker",
            Self::Workspace => "workspace",
            Self::Path => "path",
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "clipboard" => Some(Self::Clipboard),
            "drop" => Some(Self::Drop),
            "file_picker" => Some(Self::FilePicker),
            "workspace" => Some(Self::Workspace),
            "path" => Some(Self::Path),
            _ => None,
        }
    }
}

/// Where an `@`-linked file was copied from. Provenance, not a live link:
/// the workspace file may since have moved, changed or gone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WorkspaceFileOrigin {
    pub workspace_id: WorkspaceId,
    pub document_id: DocumentId,
}

/// The public metadata of one stored attachment. A path reference keeps its
/// path here because the person explicitly chose to send that path to an agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ChatAttachment {
    pub id: ChatAttachmentId,
    /// The user message it was sent with; `None` while it is staged.
    pub message_id: Option<MessageId>,
    pub ordinal: Option<u32>,
    pub kind: AttachmentKind,
    pub source: AttachmentSource,
    pub file_name: String,
    pub media_type: String,
    pub byte_count: u64,
    /// Width and height, for a picture.
    pub dimensions: Option<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<WorkspaceFileOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_path: Option<String>,
    pub created_at: Timestamp,
}

/// One row as the service keeps it: the public metadata and what only the
/// service reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentRecord {
    pub chat_id: ChatId,
    pub attachment: ChatAttachment,
    /// A smaller copy was written for the agent (`sent.png` or `sent.jpg`).
    pub has_sent_copy: bool,
    /// Lowercase hex BLAKE3 of the original, so a repeated upload of the
    /// same id can be told from a conflicting one.
    pub content_blake3: String,
}

/// An attachment as one provider request carries it. Internal: the paths
/// are on this computer and never reach a public type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AttachmentInput {
    pub id: ChatAttachmentId,
    pub kind: AttachmentKind,
    pub file_name: String,
    pub media_type: String,
    pub byte_count: u64,
    /// The stored original, which the agent is told about.
    #[cfg_attr(feature = "schema", schemars(with = "Option<String>"))]
    pub path: Option<PathBuf>,
    /// A local path reference, used instead of copied bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// For a picture: the copy to hand the agent natively (the sent copy,
    /// or the original when it was small enough to go as it is).
    #[cfg_attr(feature = "schema", schemars(with = "Option<String>"))]
    pub native: Option<PathBuf>,
}

// ----- the allowlist (D16) ------------------------------------------------

/// What a file is, by the allowlist's families.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AttachmentFamily {
    Picture,
    Pdf,
    /// `docx xlsx pptx`: a ZIP holding `[Content_Types].xml`.
    OfficeOpenXml,
    /// `odt epub`: a ZIP whose `mimetype` entry names the type.
    OpenDocument,
    RichText,
    Video,
    Text,
    /// Programming-language files: accepted, and always opened as text.
    SourceCode,
}

impl AttachmentFamily {
    /// Text and source code open as a `.txt` copy, so a click in the chat
    /// never runs a script or opens a browser.
    pub const fn opens_as_text(self) -> bool {
        matches!(self, Self::Text | Self::SourceCode)
    }
}

/// An allowed type: its family and the media type it is stored and served as.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AllowedType {
    pub family: AttachmentFamily,
    pub media_type: &'static str,
}

const SOURCE: &str = "text/plain";

const ALLOWED_EXTENSIONS: &[(&str, AttachmentFamily, &str)] = {
    use AttachmentFamily::*;
    &[
        ("png", Picture, "image/png"),
        ("jpg", Picture, "image/jpeg"),
        ("jpeg", Picture, "image/jpeg"),
        ("gif", Picture, "image/gif"),
        ("webp", Picture, "image/webp"),
        ("pdf", Pdf, "application/pdf"),
        (
            "docx",
            OfficeOpenXml,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ),
        (
            "xlsx",
            OfficeOpenXml,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ),
        (
            "pptx",
            OfficeOpenXml,
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        ),
        (
            "odt",
            OpenDocument,
            "application/vnd.oasis.opendocument.text",
        ),
        ("epub", OpenDocument, "application/epub+zip"),
        ("rtf", RichText, "application/rtf"),
        ("mp4", Video, "video/mp4"),
        ("mov", Video, "video/quicktime"),
        ("txt", Text, "text/plain"),
        ("md", Text, "text/markdown"),
        ("markdown", Text, "text/markdown"),
        ("csv", Text, "text/csv"),
        ("tsv", Text, "text/tab-separated-values"),
        ("json", Text, "application/json"),
        ("jsonl", Text, "application/jsonl"),
        ("xml", Text, "application/xml"),
        ("html", Text, "text/html"),
        ("htm", Text, "text/html"),
        ("yaml", Text, "application/yaml"),
        ("yml", Text, "application/yaml"),
        ("toml", Text, "application/toml"),
        ("ini", Text, "text/plain"),
        ("cfg", Text, "text/plain"),
        ("conf", Text, "text/plain"),
        ("log", Text, "text/plain"),
        ("c", SourceCode, SOURCE),
        ("h", SourceCode, SOURCE),
        ("cc", SourceCode, SOURCE),
        ("cpp", SourceCode, SOURCE),
        ("cxx", SourceCode, SOURCE),
        ("hpp", SourceCode, SOURCE),
        ("hh", SourceCode, SOURCE),
        ("cs", SourceCode, SOURCE),
        ("go", SourceCode, SOURCE),
        ("rs", SourceCode, SOURCE),
        ("java", SourceCode, SOURCE),
        ("kt", SourceCode, SOURCE),
        ("kts", SourceCode, SOURCE),
        ("scala", SourceCode, SOURCE),
        ("swift", SourceCode, SOURCE),
        ("m", SourceCode, SOURCE),
        ("mm", SourceCode, SOURCE),
        ("py", SourceCode, SOURCE),
        ("rb", SourceCode, SOURCE),
        ("php", SourceCode, SOURCE),
        ("js", SourceCode, SOURCE),
        ("mjs", SourceCode, SOURCE),
        ("cjs", SourceCode, SOURCE),
        ("jsx", SourceCode, SOURCE),
        ("ts", SourceCode, SOURCE),
        ("tsx", SourceCode, SOURCE),
        ("vue", SourceCode, SOURCE),
        ("svelte", SourceCode, SOURCE),
        ("css", SourceCode, SOURCE),
        ("scss", SourceCode, SOURCE),
        ("sass", SourceCode, SOURCE),
        ("less", SourceCode, SOURCE),
        ("sql", SourceCode, SOURCE),
        ("lua", SourceCode, SOURCE),
        ("dart", SourceCode, SOURCE),
        ("r", SourceCode, SOURCE),
        ("jl", SourceCode, SOURCE),
        ("ex", SourceCode, SOURCE),
        ("exs", SourceCode, SOURCE),
        ("erl", SourceCode, SOURCE),
        ("hrl", SourceCode, SOURCE),
        ("hs", SourceCode, SOURCE),
        ("ml", SourceCode, SOURCE),
        ("mli", SourceCode, SOURCE),
        ("clj", SourceCode, SOURCE),
        ("cljs", SourceCode, SOURCE),
        ("zig", SourceCode, SOURCE),
        ("nim", SourceCode, SOURCE),
        ("pl", SourceCode, SOURCE),
        ("pm", SourceCode, SOURCE),
        ("groovy", SourceCode, SOURCE),
        ("gradle", SourceCode, SOURCE),
        ("proto", SourceCode, SOURCE),
        ("graphql", SourceCode, SOURCE),
        ("gql", SourceCode, SOURCE),
        ("tex", SourceCode, SOURCE),
        ("diff", SourceCode, SOURCE),
        ("patch", SourceCode, SOURCE),
        ("ipynb", SourceCode, SOURCE),
    ]
};

/// Source files known by their whole name rather than an extension.
const ALLOWED_NAMES: &[&str] = &["Makefile", "Dockerfile", "LICENSE", "README"];

/// Executables, installers, shortcuts and shell or OS-automation scripts.
/// Every extension off the allowlist is refused anyway; these are named so
/// the refusal and the tests can cite them.
pub const REFUSED_EXTENSIONS: &[&str] = &[
    "exe", "com", "scr", "cpl", "msi", "msix", "appx", "app", "dmg", "pkg", "deb", "rpm", "apk",
    "jar", "lnk", "url", "reg", "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "jse", "wsf", "wsh",
    "hta", "sh", "bash", "zsh", "fish", "command",
];

/// The lowercase extension of a file name, without the dot; `None` for a
/// name with none (or only a leading dot, like `.env`).
pub fn extension_of(name: &str) -> Option<String> {
    let (stem, extension) = name.rsplit_once('.')?;
    if stem.is_empty() || extension.is_empty() {
        return None;
    }
    Some(extension.to_ascii_lowercase())
}

/// Whether a file of this name may be attached at all, judged by its name.
/// The UI asks this at paste, drop and `@`; the service asks it again and
/// then checks the content with [`content_agrees`].
pub fn allowed_type(name: &str) -> Option<AllowedType> {
    match extension_of(name) {
        Some(extension) => ALLOWED_EXTENSIONS
            .iter()
            .find(|(allowed, ..)| *allowed == extension)
            .map(|&(_, family, media_type)| AllowedType { family, media_type }),
        None => ALLOWED_NAMES.contains(&name).then_some(AllowedType {
            family: AttachmentFamily::SourceCode,
            media_type: SOURCE,
        }),
    }
}

/// Whether a file's first bytes agree with the family its name claims.
/// `head` is at most [`CONTENT_CHECK_BYTES`] from the start. An executable
/// header is refused whatever the name says. The ZIP families are checked
/// here by their magic only; the service also looks inside the archive.
pub fn content_agrees(family: AttachmentFamily, head: &[u8]) -> bool {
    if is_executable(head) {
        return false;
    }
    match family {
        AttachmentFamily::Picture => picture_media_type(head).is_some(),
        AttachmentFamily::Pdf => head.starts_with(b"%PDF-"),
        AttachmentFamily::OfficeOpenXml | AttachmentFamily::OpenDocument => {
            head.starts_with(b"PK\x03\x04")
        }
        AttachmentFamily::RichText => head.starts_with(b"{\\rtf"),
        AttachmentFamily::Video => head.get(4..8) == Some(b"ftyp"),
        AttachmentFamily::Text | AttachmentFamily::SourceCode => is_text(head),
    }
}

/// The media type of one of the four allowed picture formats, sniffed from
/// the bytes. A picture's stored type is what its bytes are, not its name.
pub fn picture_media_type(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(b"\xFF\xD8\xFF") {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

fn is_executable(head: &[u8]) -> bool {
    const MAGICS: &[&[u8]] = &[
        b"MZ",
        b"\x7FELF",
        b"\xFE\xED\xFA\xCE",
        b"\xFE\xED\xFA\xCF",
        b"\xCE\xFA\xED\xFE",
        b"\xCF\xFA\xED\xFE",
        b"\xCA\xFE\xBA\xBE",
    ];
    MAGICS.iter().any(|magic| head.starts_with(magic))
}

/// The text rule: valid UTF-8 with no NUL, or UTF-16 with a byte-order
/// mark. A character cut by the end of `head` is not a failure.
fn is_text(head: &[u8]) -> bool {
    let utf16 = match head {
        [0xFF, 0xFE, rest @ ..] => Some((rest, true)),
        [0xFE, 0xFF, rest @ ..] => Some((rest, false)),
        _ => None,
    };
    if let Some((rest, little_endian)) = utf16 {
        let units = rest.chunks_exact(2).map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        });
        let mut decoded: Vec<_> = char::decode_utf16(units).collect();
        // A surrogate pair cut by the end of `head` is not a failure.
        if matches!(decoded.last(), Some(Err(_))) {
            decoded.pop();
        }
        return decoded
            .into_iter()
            .all(|unit| unit.is_ok_and(|ch| ch != '\0'));
    }
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(error) => error.error_len().is_none(),
    }
}

/// A file name safe to keep on any of the three platforms: no separators,
/// control characters or `<>:"|?*`, no leading or trailing dots or spaces,
/// a Windows reserved name suffixed with `_`, at most
/// [`MAX_ATTACHMENT_NAME_CHARS`] characters with the extension kept, and
/// `attachment` for a name with nothing left.
pub fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|ch| {
            !ch.is_control() && !matches!(ch, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*')
        })
        .collect();
    let mut cleaned = cleaned
        .trim_matches(|ch: char| ch == '.' || ch.is_whitespace())
        .to_owned();
    if cleaned.chars().count() > MAX_ATTACHMENT_NAME_CHARS {
        let extension = cleaned
            .rsplit_once('.')
            .map(|(_, extension)| extension)
            .filter(|extension| !extension.is_empty() && extension.chars().count() < 16)
            .map(|extension| format!(".{extension}"))
            .unwrap_or_default();
        let keep = MAX_ATTACHMENT_NAME_CHARS - extension.chars().count();
        let stem: String = cleaned.chars().take(keep).collect();
        cleaned = format!(
            "{}{extension}",
            stem.trim_end_matches(|ch: char| ch == '.' || ch.is_whitespace())
        );
    }
    if cleaned.is_empty() {
        return "attachment".into();
    }
    let stem = cleaned
        .split_once('.')
        .map_or(cleaned.as_str(), |(stem, _)| stem);
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(stem))
    {
        cleaned.insert(stem.len(), '_');
    }
    cleaned
}

#[cfg(test)]
mod tests;
