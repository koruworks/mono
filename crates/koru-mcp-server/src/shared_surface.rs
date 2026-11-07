// SPDX-License-Identifier: MPL-2.0

//! What every Koru MCP host serves beside its tools: the instructions an agent
//! is told first, and the documentation resources.
//!
//! The local bridge ([`crate::KoruMcp`]) serves them through rmcp's server
//! handler. The hosted front door at `mcp.koru.do` uses rmcp's model types and
//! not its transport, so it cannot run that handler; it calls these instead.
//! **One source for both**, so the two hosts cannot tell an agent different
//! things: the bridge's handlers are written over the same functions.

use rmcp::model::{ErrorData, ReadResourceResult, Resource, ResourceContents, ResourceTemplate};
use serde_json::{Value, json};

use crate::{
    ESSENTIALS_RESOURCE_URI, MODEL_DOCUMENT_MIME, MODEL_RESOURCE_URI, MODEL_SECTION_URI_PREFIX,
    SERVER_INSTRUCTIONS, documentation, essentials_resource, model_resource,
    model_section_template,
};

/// The instructions every host gives an agent before its first call.
pub fn server_instructions() -> &'static str {
    SERVER_INSTRUCTIONS
}

/// `resources/list`: the essentials and the contract's table of contents.
pub fn documentation_resources() -> Vec<Resource> {
    vec![essentials_resource(), model_resource()]
}

/// `resources/templates/list`: one section of the contract by slug.
pub fn documentation_resource_templates() -> Vec<ResourceTemplate> {
    vec![model_section_template()]
}

/// `resources/read` for a documentation URI, exactly as the bridge answers it
/// when every toolset is on.
pub fn read_documentation_resource(uri: &str) -> Result<ReadResourceResult, ErrorData> {
    read_with(uri, |answer| answer)
}

/// The bridge's form: `note` adds what only it knows (which toolsets are on)
/// to the answer before its text is taken.
pub(crate) fn read_with(
    uri: &str,
    note: impl FnOnce(Value) -> Value,
) -> Result<ReadResourceResult, ErrorData> {
    let known = uri == MODEL_RESOURCE_URI
        || uri == ESSENTIALS_RESOURCE_URI
        || uri.starts_with(MODEL_SECTION_URI_PREFIX);
    if !known {
        return Err(ErrorData::resource_not_found(
            "Koru documentation resource not found",
            Some(json!({ "uri": uri })),
        ));
    }
    documentation(uri, None)
        .map(|answer| {
            let answer = note(answer);
            let text = answer["text"].as_str().unwrap_or_default().to_owned();
            ReadResourceResult::new(vec![
                ResourceContents::text(text, uri.to_owned()).with_mime_type(MODEL_DOCUMENT_MIME),
            ])
        })
        .map_err(|error| match error.code.as_str() {
            // A slug nobody wrote is the reader's to fix, and the details list
            // the slugs there are.
            "documentation_section_not_found" => {
                ErrorData::resource_not_found(error.message, error.details)
            }
            _ => ErrorData::internal_error(error.message, Some(json!({ "uri": uri }))),
        })
}
