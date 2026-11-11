// SPDX-License-Identifier: MPL-2.0

//! **MCP Apps** (`io.modelcontextprotocol/ui`): a view a host renders beside
//! the conversation, and the tool that opens it.
//!
//! A view is one self-contained HTML document served as a `ui://` resource. A
//! host that speaks the extension renders it in a sandboxed frame when the
//! model calls a tool whose `_meta.ui.resourceUri` names it, hands it the
//! tool's arguments and result, and proxies the `tools/call` requests the view
//! makes back to this server, under the connection's own authority.
//!
//! **The view is a client of the tool contract, not of a second API.** It
//! reads with `read_document` and saves with `edit_document`, the tools an
//! agent uses, so a save carries the same preconditions, the same attribution
//! and the same refusals, and there is nothing here to keep in step with
//! them. The view's own code talks to one function, `call(tool, arguments)`;
//! in a host that function is `tools/call` over `postMessage`.
//!
//! **A view tool is a presentation of a tool the executor already has**, not
//! a tool of its own: [`executes_as`] names the one it runs as, and
//! [`present`] shapes that answer for a host. So `show_document` is
//! `read_document` everywhere below the surface — capabilities, scope,
//! workspace authority, a hosted replica's resident bodies and claims — and
//! only the two surfaces that publish `tools/list` know its name: the stdio
//! bridge and the hosted front door. Koru AI reads
//! [`crate::workspace_tool_catalog`], which does not list it, because Koru AI
//! renders no views.
//!
//! **A host without the extension loses nothing.** It ignores `_meta.ui`, and
//! the tool's text says to use `read_document`.
//!
//! **`read_image` is the one tool a view has to itself.** A view may load
//! nothing from the network, so a document's image reaches it as the answer
//! to a tool call: one image block, scaled for display, as an MCP image
//! content block. It is a tool of the executor, because it reads a blob, but
//! it is in no router and no catalog, and is published for the view alone
//! (`_meta.ui.visibility: ["app"]`). A hosted server serves it too, fetching
//! the bytes from Cloud.

use std::sync::LazyLock;

use rmcp::model::{
    CallToolResult, ContentBlock, MetaObject, ReadResourceResult, Resource, ResourceContents, Tool,
    ToolAnnotations,
};
use serde_json::{Value, json};

use crate::ToolError;

/// The MIME type that marks a resource as an MCP App view.
pub const APP_VIEW_MIME: &str = "text/html;profile=mcp-app";

/// Shows one document to the person, in a view they can edit and save.
pub const SHOW_DOCUMENT: &str = "show_document";

/// One image block of a document, scaled for display, for the view.
pub const READ_IMAGE: &str = "read_image";

/// What every revision of the document view's URI starts with.
const DOCUMENT_VIEW_URI_PREFIX: &str = "ui://koru/document";

const DOCUMENT_VIEW_HTML: &str = include_str!("apps/document.html");

/// The document view's URI, which names its content: hosts keep a view by its
/// URI, so a changed view under an unchanged URI would go on being served
/// from a host's copy.
static DOCUMENT_VIEW_URI: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{DOCUMENT_VIEW_URI_PREFIX}-{:08x}.html",
        fnv1a(DOCUMENT_VIEW_HTML.as_bytes())
    )
});

/// FNV-1a, 32 bits: a name for the view's bytes, not a security property.
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
    })
}

/// The URI `show_document` points a host at.
pub fn document_view_uri() -> &'static str {
    &DOCUMENT_VIEW_URI
}

/// The executor tool a view tool runs as, or `None` for any other name.
pub fn executes_as(tool: &str) -> Option<&'static str> {
    match tool {
        SHOW_DOCUMENT => Some("read_document"),
        READ_IMAGE => Some(READ_IMAGE),
        _ => None,
    }
}

/// The catalog tool a view tool is listed with: a bridge lists a view tool
/// while that tool's toolset is on, and refuses it with that toolset's
/// refusal when it is off. `None` for any other name.
pub fn listed_with(tool: &str) -> Option<&'static str> {
    matches!(tool, SHOW_DOCUMENT | READ_IMAGE).then_some("read_document")
}

/// The view tools, as `tools/list` publishes them beside the catalog.
pub fn app_tools() -> Vec<Tool> {
    let input = json!({
        "type": "object",
        "properties": {
            "workspace_id": { "type": "string" },
            "document_id": { "type": "string" },
        },
        "required": ["workspace_id", "document_id"],
        "additionalProperties": false,
    });
    let meta = json!({
        "ui": { "resourceUri": document_view_uri(), "visibility": ["model", "app"] },
    });
    vec![
        Tool::new(
            SHOW_DOCUMENT,
            "Show the user one document in a Koru view they can read, edit and save: text, headings, checklists and tasks. For \"show me\" or \"open\" a note, document or task list. To read content yourself, use read_document.",
            std::sync::Arc::new(object(input)),
        )
        .with_title("Show a Koru document")
        .with_annotations(ToolAnnotations::new().read_only(true).open_world(false))
        .with_meta(MetaObject(object(meta))),
        Tool::new(
            READ_IMAGE,
            "One image block, scaled, for the Koru document view.",
            std::sync::Arc::new(object(json!({
                "type": "object",
                "properties": {
                    "workspace_id": { "type": "string" },
                    "document_id": { "type": "string" },
                    "node_id": { "type": "string" },
                },
                "required": ["workspace_id", "document_id", "node_id"],
                "additionalProperties": false,
            }))),
        )
        .with_title("Read a Koru image")
        .with_annotations(ToolAnnotations::new().read_only(true).open_world(false))
        .with_meta(MetaObject(object(json!({ "ui": { "visibility": ["app"] } })))),
    ]
}

/// A view tool's answer, from the answer of the tool it ran as.
///
/// The structured content stays what the view renders from: the
/// `read_document` projection, with the `workspace_id` the call named so the
/// result alone says where its document is. **The text is a sentence, not the
/// document**: a model that is shown the text block (Claude reads
/// `structuredContent` only when there is no text) is told what the person is
/// looking at without paying for content it did not ask to read. A refusal is
/// passed on as it is.
///
/// An image is answered as an MCP image content block, with its bytes taken
/// out of the structured content, which keeps `{node_id, mime, width,
/// height}`.
pub fn present(tool: &str, arguments: &Value, mut result: CallToolResult) -> CallToolResult {
    if result.is_error == Some(true) {
        return result;
    }
    if tool == READ_IMAGE {
        if let Some(Value::Object(image)) = &mut result.structured_content
            && let Some(Value::String(data)) = image.remove("base64")
        {
            let mime = image
                .get("mime")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            result.content = vec![ContentBlock::image(data, mime)];
        }
        return result;
    }
    if tool != SHOW_DOCUMENT {
        return result;
    }
    let Some(Value::Object(document)) = &mut result.structured_content else {
        return result;
    };
    if let Some(workspace) = arguments.get("workspace_id").and_then(Value::as_str) {
        document.insert("workspace_id".into(), workspace.into());
    }
    let nodes = document.get("nodes").and_then(Value::as_array);
    let title = nodes
        .and_then(|nodes| nodes.first())
        .and_then(|root| root["content"]["text"].as_str())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("Untitled");
    let blocks = nodes.map_or(0, |nodes| nodes.len().saturating_sub(1));
    result.content = vec![ContentBlock::text(format!(
        "Showing \"{title}\" ({blocks} blocks) to the user in a Koru view. They can edit and save it there, so call read_document for its current content and revision before you quote or change it. If no view appeared, this host does not render MCP Apps: use read_document instead."
    ))];
    result
}

/// `resources/list`: the views.
pub fn app_resources() -> Vec<Resource> {
    vec![
        Resource::new(document_view_uri(), "koru-document-view")
            .with_title("Koru document view")
            .with_description(
                "The interactive view show_document opens: one document, editable, saved through edit_document.",
            )
            .with_mime_type(APP_VIEW_MIME),
    ]
}

/// `resources/read` for a view, or `None` when `uri` names no view.
///
/// Any revision's URI answers the current view: a host that kept an older
/// `tools/list` asks for the URI it was given, and the view it gets speaks to
/// the tools this server has now.
pub fn read_app_resource(uri: &str) -> Option<ReadResourceResult> {
    uri.starts_with(DOCUMENT_VIEW_URI_PREFIX).then(|| {
        // No `csp` and no `permissions`: the view loads nothing and calls
        // nothing but its host, so the host's default policy (no network at
        // all) is the right one.
        let meta = json!({ "ui": { "prefersBorder": true } });
        ReadResourceResult::new(vec![
            ResourceContents::text(DOCUMENT_VIEW_HTML, uri.to_owned())
                .with_mime_type(APP_VIEW_MIME)
                .with_meta(MetaObject(object(meta))),
        ])
    })
}

/// An image as the view is sent it.
pub(crate) struct Preview {
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

/// An image no larger than this, in a format a browser draws, is sent as it
/// is stored, so a small GIF keeps its frames.
const PREVIEW_AS_STORED_BYTES: usize = 256 * 1024;

/// The longest edge of an image that is scaled down for the view.
const PREVIEW_EDGE: u32 = 1280;

/// The largest image a preview is decoded from, on either edge.
const PREVIEW_SOURCE_EDGE: u32 = 16_384;

/// A stored image as a view shows it: as stored when it is small, and
/// otherwise scaled to fit [`PREVIEW_EDGE`] and encoded again, as PNG when it
/// has transparency and JPEG when it has none. A view's image crosses two
/// processes and a `postMessage` as base64 inside JSON, so a photograph is
/// not sent at the size it was taken.
pub(crate) fn image_preview(bytes: Vec<u8>, mime: &str) -> Result<Preview, ToolError> {
    use image::ImageReader;
    use std::io::Cursor;

    let unreadable = |reason: String| ToolError {
        code: "image_not_previewable".into(),
        message: format!("this image cannot be shown in a view: {reason}"),
        details: None,
    };
    let reader = || {
        let mut reader = ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|error| unreadable(error.to_string()))?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(PREVIEW_SOURCE_EDGE);
        limits.max_image_height = Some(PREVIEW_SOURCE_EDGE);
        reader.limits(limits);
        Ok::<_, ToolError>(reader)
    };
    let as_stored = match mime {
        "image/png" => Some("image/png"),
        "image/jpeg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        _ => None,
    };
    if let Some(mime) = as_stored
        && bytes.len() <= PREVIEW_AS_STORED_BYTES
    {
        let (width, height) = reader()?
            .into_dimensions()
            .map_err(|error| unreadable(error.to_string()))?;
        return Ok(Preview {
            mime,
            width,
            height,
            bytes,
        });
    }
    let mut image = reader()?
        .decode()
        .map_err(|error| unreadable(error.to_string()))?;
    if image.width() > PREVIEW_EDGE || image.height() > PREVIEW_EDGE {
        image = image.thumbnail(PREVIEW_EDGE, PREVIEW_EDGE);
    }
    let mut out = Vec::new();
    let mime = if image.color().has_alpha() {
        image
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|error| unreadable(error.to_string()))?;
        "image/png"
    } else {
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
        image
            .to_rgb8()
            .write_with_encoder(encoder)
            .map_err(|error| unreadable(error.to_string()))?;
        "image/jpeg"
    };
    Ok(Preview {
        mime,
        width: image.width(),
        height: image.height(),
        bytes: out,
    })
}

fn object(value: Value) -> rmcp::model::JsonObject {
    match value {
        Value::Object(object) => object,
        _ => rmcp::model::JsonObject::new(),
    }
}

#[cfg(test)]
mod tests;
