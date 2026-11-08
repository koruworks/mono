// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{HostCapabilities, workspace_tool_catalog};

fn tool(catalog: &[WorkspaceTool], name: &str) -> WorkspaceTool {
    catalog
        .iter()
        .find(|tool| tool.name == name)
        .cloned()
        .unwrap_or_else(|| panic!("{name} is published"))
}

/// TECHNICAL §5.4 item 1: the hosted catalog adds an optional `claim_ids` to
/// `edit_document` and `extract_to_document` — at most two handles — and
/// changes nothing else of either schema. The local one is untouched.
#[test]
fn the_hosted_edit_and_extract_take_claim_ids_and_the_local_ones_do_not() {
    let shared = workspace_tool_catalog();
    let hosted = HostCapabilities::rung1().catalog();
    for name in CLAIMING_TOOLS {
        let local = tool(&shared, name);
        let served = tool(&hosted, name);
        assert!(local.input_schema["properties"].get("claim_ids").is_none());
        let claim_ids = &served.input_schema["properties"]["claim_ids"];
        assert_eq!(claim_ids["type"], "array", "{name}");
        assert_eq!(claim_ids["maxItems"], MAX_CLAIM_IDS, "{name}");
        assert_eq!(claim_ids["items"]["type"], "string", "{name}");
        assert_eq!(
            claim_ids["items"]["maxLength"], MAX_CLAIM_ID_BYTES,
            "{name}"
        );
        // Optional, and the schema is otherwise the local one.
        assert_eq!(
            served.input_schema["required"], local.input_schema["required"],
            "{name}"
        );
        let mut without = served.input_schema.clone();
        without["properties"]
            .as_object_mut()
            .unwrap()
            .remove("claim_ids");
        assert_eq!(without, local.input_schema, "{name}");
        assert!(served.description.starts_with(&local.description));
        assert!(served.description.contains("claim_ids"));
        assert_eq!(served.safety, local.safety);
    }
    for name in HOSTED_ONLY_TOOLS {
        assert!(shared.iter().all(|tool| tool.name != *name), "{name}");
    }
}

/// `claim_document` and `release_document` as `hosted-tools-v1` §4 lists
/// their inputs, each closed, and each description stating the boundary.
#[test]
fn the_claim_tools_publish_the_contract_s_inputs_and_boundary() {
    let tools = claim_tools();
    let claim = tool(&tools, CLAIM_DOCUMENT);
    let properties = claim.input_schema["properties"].as_object().unwrap();
    let mut names: Vec<_> = properties.keys().cloned().collect();
    names.sort();
    assert_eq!(
        names,
        [
            "agent_label",
            "claim_id",
            "document_id",
            "duration_seconds",
            "purpose",
            "workspace_id"
        ]
    );
    assert_eq!(
        claim.input_schema["required"],
        json!(["workspace_id", "document_id"])
    );
    assert_eq!(properties["duration_seconds"]["minimum"], 60);
    assert_eq!(properties["duration_seconds"]["maximum"], 900);
    assert_eq!(properties["duration_seconds"]["default"], 300);
    assert_eq!(properties["purpose"]["maxLength"], 120);
    assert_eq!(properties["agent_label"]["maxLength"], 64);
    let release = tool(&tools, RELEASE_DOCUMENT);
    assert_eq!(release.input_schema["required"], json!(["claim_id"]));
    assert!(release.safety.idempotent);
    for tool in &tools {
        assert_eq!(tool.input_schema["additionalProperties"], false);
        assert!(!tool.safety.read_only && !tool.safety.destructive);
        assert!(tool.description.contains(BOUNDARY), "{}", tool.name);
        assert!(tool.description.contains("local MCP"));
    }
    assert!(claim.description.contains("do not work around the claim"));
}
