// SPDX-License-Identifier: MPL-2.0

//! **What only a hosted catalog has**: document claims (Cloud MCP
//! TECHNICAL §5.4 item 1 and §5.7; `contracts/hosted-tools-v1.md` §4).
//!
//! A claim is an exclusive, expiring lease one hosted agent holds on one
//! document. The two claim tools and the `claim_ids` handles on
//! `edit_document`, `extract_to_document`, `add_image` and `replace_image` exist only where the server is
//! the hosted one: a claim is held by Koru Cloud, and nothing a device does
//! is ever blocked by one. **The local catalog is unchanged**: these tools are
//! not in the shared router, a local host refuses them by name as
//! `tool_not_available`, and the local `edit_document` and
//! `extract_to_document` keep their input schemas.
//!
//! This crate publishes the tools and admits them (capabilities, scope,
//! workspace authority); the hosted server implements them, because only it
//! can reach the claims.

use serde_json::{Value, json};

use crate::{ToolSafety, WorkspaceTool};

pub const CLAIM_DOCUMENT: &str = "claim_document";
pub const RELEASE_DOCUMENT: &str = "release_document";

/// The tools only a hosted catalog serves.
pub const HOSTED_ONLY_TOOLS: &[&str] = &[CLAIM_DOCUMENT, RELEASE_DOCUMENT];

/// The mutating tools that take `claim_ids` on a hosted catalog.
pub const CLAIMING_TOOLS: &[&str] = &[
    "edit_document",
    "extract_to_document",
    "add_image",
    "replace_image",
];

/// The handles one call may present: an extraction's source and destination.
pub const MAX_CLAIM_IDS: usize = 2;

/// The longest handle anyone may present, in bytes (the API mints 43).
pub const MAX_CLAIM_ID_BYTES: usize = 128;

/// What every claim tool's description says about where a claim stops.
const BOUNDARY: &str = "A claim stops every hosted agent's change that Koru Cloud accepts after the claim is granted. It does not stop people on their devices, local MCP, Koru AI or delegated runs; the document's merging keeps all of those safe.";

/// The sentence a hosted `edit_document` and `extract_to_document` add.
const CLAIM_IDS_NOTE: &str = " Hosted: when a document this call changes is claimed (read_document and list_documents show `claim`), pass the claim's handle in claim_ids (at most 2, one per claimed document); a claimed document without its handle is refused document_claimed and nothing is written.";

fn handle() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": MAX_CLAIM_ID_BYTES })
}

/// The two claim tools, as a hosted catalog publishes them.
pub(crate) fn claim_tools() -> Vec<WorkspaceTool> {
    vec![
        WorkspaceTool {
            name: CLAIM_DOCUMENT.into(),
            title: "Claim a document".into(),
            description: format!(
                "Take an exclusive, expiring claim on one document for this agent, or renew one by passing its claim_id. Requires Read & edit. While it lives, other hosted agents' changes to the document are refused — on other connections, and on this one without the handle — so pass the returned claim_id in claim_ids on edit_document and extract_to_document. duration_seconds is 60–900 (default 300); purpose (up to 120 characters) and agent_label (up to 64) are self-declared and shown to other agents as such. Claiming a document another agent holds answers document_claimed with who holds it and until when: wait until expires_at or retry later, and do not work around the claim. Release it with release_document when done. {BOUNDARY}"
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workspace_id": { "type": "string" },
                    "document_id": { "type": "string" },
                    "claim_id": handle(),
                    "purpose": { "type": "string", "minLength": 1, "maxLength": 120 },
                    "agent_label": { "type": "string", "minLength": 1, "maxLength": 64 },
                    "duration_seconds": {
                        "type": "integer",
                        "minimum": 60,
                        "maximum": 900,
                        "default": 300,
                    },
                },
                "required": ["workspace_id", "document_id"],
                "additionalProperties": false,
            }),
            output_schema: json!({ "type": "object" }),
            safety: ToolSafety {
                read_only: false,
                destructive: false,
                idempotent: false,
                open_world: false,
            },
        },
        WorkspaceTool {
            name: RELEASE_DOCUMENT.into(),
            title: "Release a document claim".into(),
            description: format!(
                "Release a claim taken with claim_document, by its claim_id, so other agents may change the document again. Idempotent: a claim already released or expired answers the same. {BOUNDARY}"
            ),
            input_schema: json!({
                "type": "object",
                "properties": { "claim_id": handle() },
                "required": ["claim_id"],
                "additionalProperties": false,
            }),
            output_schema: json!({ "type": "object" }),
            safety: ToolSafety {
                read_only: false,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
        },
    ]
}

/// A shared tool as a hosted catalog publishes it: `edit_document` and
/// `extract_to_document` gain the optional `claim_ids`; every other tool is
/// unchanged.
pub(crate) fn hosted_tool(mut tool: WorkspaceTool) -> WorkspaceTool {
    if !CLAIMING_TOOLS.contains(&tool.name.as_str()) {
        return tool;
    }
    if let Some(properties) = tool
        .input_schema
        .as_object_mut()
        .map(|schema| schema.entry("properties").or_insert_with(|| json!({})))
        .and_then(Value::as_object_mut)
    {
        properties.insert(
            "claim_ids".into(),
            json!({ "type": "array", "items": handle(), "maxItems": MAX_CLAIM_IDS }),
        );
    }
    tool.description.push_str(CLAIM_IDS_NOTE);
    tool
}

#[cfg(test)]
mod tests;
