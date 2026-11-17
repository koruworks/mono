// SPDX-License-Identifier: MPL-2.0

//! The fake's own front door, asked things the client never sends, and the
//! one claim the fake makes about itself: its catalog is the shared crate's.

use serde_json::{Value, json};

use super::*;
use crate::protocol::PROTOCOL_REVISION;

const META: &str = "io.modelcontextprotocol/protocolVersion";

fn body(method: &str, params: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .unwrap()
}

fn rpc_code(response: &PostResponse) -> i64 {
    let message: Value = serde_json::from_slice(&response.body).expect("a JSON-RPC error");
    message["error"]["code"].as_i64().expect("an error code")
}

#[test]
fn a_header_that_differs_from_the_body_is_refused_before_anything_runs() {
    let cloud = FakeCloud::new();
    let connection = cloud.connect("first", Access::ReadAndEdit);
    let token = || connection.access_token().unwrap();
    let meta = json!({ META: PROTOCOL_REVISION });

    // The name header names another tool.
    let request = PostRequest::new(
        "tools/call",
        Some("read_document".into()),
        PROTOCOL_REVISION,
        token(),
        body(
            "tools/call",
            json!({ "name": "edit_document", "arguments": {}, "_meta": meta }),
        ),
    );
    let response = connection.post(&request).unwrap();
    assert_eq!((response.status, rpc_code(&response)), (400, -32020));

    // The method header names another method.
    let request = PostRequest::new(
        "tools/list",
        None,
        PROTOCOL_REVISION,
        token(),
        body(
            "tools/call",
            json!({ "name": "read_document", "arguments": {}, "_meta": meta }),
        ),
    );
    let response = connection.post(&request).unwrap();
    assert_eq!((response.status, rpc_code(&response)), (400, -32020));

    // The revision header is one the server speaks, and not the body's.
    let request = PostRequest::new(
        "tools/list",
        None,
        "2025-11-25",
        token(),
        body("tools/list", json!({ "_meta": meta })),
    );
    let response = connection.post(&request).unwrap();
    assert_eq!((response.status, rpc_code(&response)), (400, -32020));

    // A method the server does not implement.
    let request = PostRequest::new(
        "prompts/list",
        None,
        PROTOCOL_REVISION,
        token(),
        body("prompts/list", json!({ "_meta": meta })),
    );
    let response = connection.post(&request).unwrap();
    assert_eq!((response.status, rpc_code(&response)), (404, -32601));

    // A body that is not JSON.
    let request = PostRequest::new(
        "tools/list",
        None,
        PROTOCOL_REVISION,
        token(),
        b"tools, please".to_vec(),
    );
    let response = connection.post(&request).unwrap();
    assert_eq!((response.status, rpc_code(&response)), (400, -32700));
}

#[test]
fn a_token_the_server_does_not_hold_is_challenged_with_no_real_address() {
    let cloud = FakeCloud::new();
    let connection = cloud.connect("first", Access::ReadAndEdit);
    let request = PostRequest::new(
        "tools/list",
        None,
        PROTOCOL_REVISION,
        AccessToken::new("somebody-elses").unwrap(),
        body(
            "tools/list",
            json!({ "_meta": { META: PROTOCOL_REVISION } }),
        ),
    );
    let response = connection.post(&request).unwrap();
    assert_eq!(response.status, 401);
    let challenge = response.www_authenticate.expect("a challenge");
    assert!(challenge.starts_with("Bearer resource_metadata="));
    assert!(
        challenge.contains("//fake.invalid/"),
        "the fake names no real host"
    );
    assert!(response.body.is_empty());
}

#[test]
fn the_fakes_catalog_is_the_shared_crates_hosted_catalog() {
    let cloud = FakeCloud::new();
    let client = cloud.connect("first", Access::ReadAndEdit).client();
    let listed = client.list_tools().unwrap();
    let published = HostCapabilities::rung1().catalog();
    assert_eq!(listed.len(), published.len());
    for (listed, published) in listed.iter().zip(&published) {
        assert_eq!(listed.name, published.name);
        let definition = listed.definition().expose();
        assert_eq!(
            definition["inputSchema"], published.input_schema,
            "{}",
            published.name
        );
        assert_eq!(definition["description"], published.description);
    }
}

#[test]
fn a_fate_is_spent_by_the_call_it_was_armed_for() {
    let cloud = FakeCloud::new();
    let connection = cloud.connect("first", Access::ReadAndEdit);
    let client = connection.client();
    let workspace = cloud.workspace_id();
    let create = |title: &str| {
        client.write(
            crate::WriteTool::CreateDocument,
            json!({ "workspace_id": workspace, "title": title }),
        )
    };
    // Armed past the call's last submission: the call saves, and the script
    // does not wait for a later one.
    cloud.arm(5, Fate::NotSaved);
    assert_eq!(create("First").code(), "saved");
    assert_eq!(create("Second").code(), "saved");
    // A read in between does not spend it.
    cloud.arm(0, Fate::NotSaved);
    client.list_workspaces().unwrap();
    assert_eq!(create("Third").code(), "not_saved");
    assert_eq!(create("Fourth").code(), "saved");
}
