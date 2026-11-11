// SPDX-License-Identifier: MPL-2.0

use std::sync::{Arc, Mutex};

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams};

use super::*;
use crate::{Backend, KoruMcp, RequestContext, ToolError, Toolsets};

const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
const DOCUMENT: &str = "33333333-3333-4333-8333-333333333333";

/// A `read_document` projection, as small as one can be.
fn projection(title: &str) -> Value {
    json!({
        "document_id": DOCUMENT,
        "root_node_id": "00000000-0000-4000-8000-000000000001",
        "revision": "00",
        "schema_version": 1,
        "nodes": [
            { "node_id": "00000000-0000-4000-8000-000000000001", "parent_id": null,
              "kind": "document", "content": { "type": "text", "text": title } },
            { "node_id": "00000000-0000-4000-8000-000000000002",
              "parent_id": "00000000-0000-4000-8000-000000000001",
              "kind": "paragraph", "content": { "type": "text", "text": "Milk" } },
        ],
    })
}

/// Answers `read_document` with a projection and records what it was asked.
#[derive(Default)]
struct Reads {
    asked: Mutex<Vec<String>>,
}

impl Backend for Reads {
    fn invoke(&self, _: &RequestContext, tool: &str, _: Value) -> Result<Value, ToolError> {
        self.asked.lock().unwrap().push(tool.to_owned());
        Ok(projection("Groceries"))
    }

    fn doctor(&self) -> Result<Value, ToolError> {
        Ok(json!({ "ok": true }))
    }
}

fn arguments() -> rmcp::model::JsonObject {
    object(json!({ "workspace_id": WORKSPACE, "document_id": DOCUMENT }))
}

#[test]
fn the_view_tool_names_its_view_and_the_tool_it_runs_as() {
    let tools = app_tools();
    assert_eq!(tools.len(), 3);
    let tool = serde_json::to_value(&tools[0]).expect("tool JSON");
    assert_eq!(tool["name"], SHOW_DOCUMENT);
    assert_eq!(tool["_meta"]["ui"]["resourceUri"], document_view_uri());
    assert_eq!(tool["_meta"]["ui"]["visibility"], json!(["model", "app"]));
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(
        tool["inputSchema"]["required"],
        json!(["workspace_id", "document_id"])
    );
    assert_eq!(executes_as(SHOW_DOCUMENT), Some("read_document"));
    assert_eq!(executes_as("read_document"), None);
    assert_eq!(listed_with(SHOW_DOCUMENT), Some("read_document"));
    assert_eq!(listed_with("read_document"), None);

    // The view's own tool: a tool of the executor, published for the view
    // alone and listed with the documents.
    let image = serde_json::to_value(&tools[1]).expect("tool JSON");
    assert_eq!(image["name"], READ_IMAGE);
    assert_eq!(image["_meta"]["ui"]["visibility"], json!(["app"]));
    assert!(image["_meta"]["ui"].get("resourceUri").is_none());
    assert_eq!(image["annotations"]["readOnlyHint"], true);
    assert_eq!(
        image["inputSchema"]["required"],
        json!(["workspace_id", "document_id", "node_id"])
    );
    assert_eq!(executes_as(READ_IMAGE), Some(READ_IMAGE));
    assert_eq!(listed_with(READ_IMAGE), Some("read_document"));
    let image_bytes = serde_json::to_vec(&tools[1]).expect("tool JSON").len();
    println!("read_image: {image_bytes} bytes in tools/list");
    assert!(
        image_bytes < 450,
        "read_image publishes {image_bytes} bytes"
    );

    // The view's wait for a change: the same, and held open by the bridge.
    let watch = serde_json::to_value(&tools[2]).expect("tool JSON");
    assert_eq!(watch["name"], WATCH_DOCUMENT);
    assert_eq!(watch["_meta"]["ui"]["visibility"], json!(["app"]));
    assert_eq!(
        watch["inputSchema"]["required"],
        json!(["workspace_id", "document_id", "revision"])
    );
    assert_eq!(executes_as(WATCH_DOCUMENT), Some(WATCH_DOCUMENT));
    assert_eq!(listed_with(WATCH_DOCUMENT), Some("read_document"));

    // The three together are a line of the `tools/list` budget of their own.
    let all: usize = tools
        .iter()
        .map(|tool| serde_json::to_vec(tool).expect("tool JSON").len() + 1)
        .sum();
    println!("the view tools: {all} bytes in tools/list");
    assert!(
        all <= crate::toolsets::VIEW_TOOLS_BUDGET,
        "the view tools publish {all} bytes, over their budget"
    );
    // The tool is a line in every agent's context: it stays a small one.
    let bytes = serde_json::to_vec(&tools[0]).expect("tool JSON").len();
    println!("show_document: {bytes} bytes in tools/list");
    assert!(bytes < 700, "show_document publishes {bytes} bytes");
}

#[test]
fn the_view_is_served_as_an_mcp_app_under_any_of_its_revisions() {
    let uri = document_view_uri();
    assert!(uri.starts_with("ui://koru/document-") && uri.ends_with(".html"));
    let listed = serde_json::to_value(app_resources()).expect("resource JSON");
    assert_eq!(listed[0]["uri"], uri);
    assert_eq!(listed[0]["mimeType"], APP_VIEW_MIME);

    let read = read_app_resource(uri).expect("the view");
    let contents = serde_json::to_value(read.contents).expect("contents JSON");
    assert_eq!(contents[0]["uri"], uri);
    assert_eq!(contents[0]["mimeType"], "text/html;profile=mcp-app");
    assert_eq!(contents[0]["_meta"]["ui"]["prefersBorder"], true);
    assert!(
        contents[0]["_meta"]["ui"].get("csp").is_none(),
        "the view asks for no network"
    );
    let html = contents[0]["text"].as_str().expect("the document");
    assert!(html.starts_with("<!doctype html>"));

    // A host that kept an older tools/list asks for the URI it was given.
    let older = read_app_resource("ui://koru/document-00000000.html").expect("an older revision");
    let older = serde_json::to_value(older.contents).expect("contents JSON");
    assert_eq!(older[0]["uri"], "ui://koru/document-00000000.html");
    assert_eq!(older[0]["text"], contents[0]["text"]);

    assert!(read_app_resource("koru://docs/model").is_none());
    assert!(read_app_resource("ui://other/view.html").is_none());
}

/// What the view's resource metadata promises a host: nothing is loaded and
/// nothing is called but the host itself, and note text is never markup.
#[test]
fn the_view_is_self_contained_and_never_writes_markup() {
    let html = DOCUMENT_VIEW_HTML;
    for banned in [
        " src=",
        " href=",
        "fetch(",
        "XMLHttpRequest",
        "WebSocket",
        "import(",
        "@import",
        "url(",
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "eval(",
    ] {
        assert!(!html.contains(banned), "the view contains {banned:?}");
    }
}

#[test]
fn a_shown_document_is_the_projection_with_a_sentence_for_the_model() {
    let asked = json!({ "workspace_id": WORKSPACE, "document_id": DOCUMENT });
    let shown = present(
        SHOW_DOCUMENT,
        &asked,
        CallToolResult::structured(projection("Groceries")),
    );
    let structured = shown.structured_content.expect("the projection");
    assert_eq!(structured["workspace_id"], WORKSPACE);
    assert_eq!(structured["nodes"].as_array().map(Vec::len), Some(2));
    let text = serde_json::to_string(&shown.content).expect("content JSON");
    assert!(
        text.contains("Groceries") && text.contains("1 blocks"),
        "{text}"
    );
    assert!(text.contains("read_document"), "{text}");
    assert!(
        !text.contains("Milk"),
        "the text is not the document: {text}"
    );

    let untitled = present(
        SHOW_DOCUMENT,
        &asked,
        CallToolResult::structured(projection("  ")),
    );
    let text = serde_json::to_string(&untitled.content).expect("content JSON");
    assert!(text.contains("Untitled"), "{text}");

    // A refusal, and any other tool's answer, pass through untouched.
    let refusal = CallToolResult::structured_error(json!({ "code": "permission_denied" }));
    assert_eq!(present(SHOW_DOCUMENT, &asked, refusal.clone()), refusal);
    let read = CallToolResult::structured(projection("Groceries"));
    assert_eq!(present("read_document", &asked, read.clone()), read);
}

#[tokio::test]
async fn a_client_lists_the_view_tool_reads_its_view_and_is_shown_a_document() {
    let backend = Arc::new(Reads::default());
    let (server_transport, client_transport) = tokio::io::duplex(256 * 1024);
    let server = KoruMcp::new(backend.clone(), RequestContext::local());
    let server_task = tokio::spawn(async move { server.serve(server_transport).await });
    let client = ().serve(client_transport).await.expect("MCP initialize handshake");

    let tools = client.peer().list_all_tools().await.expect("tools/list");
    let show = tools
        .iter()
        .find(|tool| tool.name == SHOW_DOCUMENT)
        .expect("show_document is listed");
    let uri = serde_json::to_value(show).expect("tool JSON")["_meta"]["ui"]["resourceUri"]
        .as_str()
        .expect("a view URI")
        .to_owned();

    let view = client
        .peer()
        .read_resource(ReadResourceRequestParams::new(uri.clone()))
        .await
        .expect("resources/read of the view");
    let view = serde_json::to_value(view.contents).expect("contents JSON");
    assert_eq!(view[0]["mimeType"], APP_VIEW_MIME);
    assert!(
        view[0]["text"]
            .as_str()
            .is_some_and(|html| html.contains("ui/initialize"))
    );

    let shown = client
        .peer()
        .call_tool(CallToolRequestParams::new(SHOW_DOCUMENT).with_arguments(arguments()))
        .await
        .expect("tools/call show_document");
    assert_eq!(shown.is_error, Some(false));
    assert_eq!(
        shown.structured_content.as_ref().expect("the projection")["workspace_id"],
        WORKSPACE
    );
    // The executor ran the tool the view tool presents, under its own name.
    assert_eq!(*backend.asked.lock().unwrap(), ["read_document"]);

    client.cancel().await.expect("close MCP client");
    server_task.abort();
}

/// Holds every `watch_document` until it is told to answer, and answers
/// everything else at once.
struct Held {
    release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl Backend for Held {
    fn invoke(&self, _: &RequestContext, tool: &str, _: Value) -> Result<Value, ToolError> {
        if tool == WATCH_DOCUMENT {
            let release = self.release.lock().unwrap().take().expect("one watch");
            release
                .recv_timeout(std::time::Duration::from_secs(20))
                .expect("the test lets the watch go");
            return Ok(json!({ "changed": true, "revision": "01" }));
        }
        Ok(projection("Groceries"))
    }

    fn doctor(&self) -> Result<Value, ToolError> {
        Ok(json!({ "ok": true }))
    }
}

/// **A view's wait does not hold up the calls it is waiting to hear of.** The
/// wait is open for seconds; the save an assistant makes meanwhile, and the
/// view's own reads, are answered beside it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_watch_does_not_hold_up_other_calls() {
    let (let_go, release) = std::sync::mpsc::channel();
    let backend = Arc::new(Held {
        release: Mutex::new(Some(release)),
    });
    let (server_transport, client_transport) = tokio::io::duplex(256 * 1024);
    let server = KoruMcp::new(backend, RequestContext::local());
    let server_task = tokio::spawn(async move { server.serve(server_transport).await });
    let client = ().serve(client_transport).await.expect("MCP initialize handshake");

    let peer = client.peer().clone();
    let watching = tokio::spawn(async move {
        let mut asked = arguments();
        asked.insert("revision".into(), "00".into());
        peer.call_tool(CallToolRequestParams::new(WATCH_DOCUMENT).with_arguments(asked))
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(!watching.is_finished(), "the watch is held open");

    let read = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client
            .peer()
            .call_tool(CallToolRequestParams::new("read_document").with_arguments(arguments())),
    )
    .await
    .expect("a read is answered while the watch waits")
    .expect("tools/call read_document");
    assert_eq!(read.is_error, Some(false));
    assert!(!watching.is_finished(), "and the watch is still waiting");

    let_go.send(()).expect("the watch is listening");
    let answered = watching.await.expect("the watch task").expect("tools/call");
    assert_eq!(
        answered.structured_content.expect("the answer")["changed"],
        true
    );

    client.cancel().await.expect("close MCP client");
    server_task.abort();
}

#[tokio::test]
async fn the_view_tool_follows_the_toolset_of_the_tool_it_runs_as() {
    let (server_transport, client_transport) = tokio::io::duplex(256 * 1024);
    let server = KoruMcp::new(Arc::new(Reads::default()), RequestContext::local())
        .with_toolsets(Toolsets::parse("tasks").expect("tasks"));
    let server_task = tokio::spawn(async move { server.serve(server_transport).await });
    let client = ().serve(client_transport).await.expect("MCP initialize handshake");

    let tools = client.peer().list_all_tools().await.expect("tools/list");
    assert!(
        !tools.iter().any(|tool| listed_with(&tool.name).is_some()),
        "docs is off, so its view tools are not listed"
    );
    let image = client
        .peer()
        .call_tool(CallToolRequestParams::new(READ_IMAGE).with_arguments(arguments()))
        .await
        .expect("a tool result, not a protocol error");
    assert_eq!(
        image.structured_content.expect("the refusal")["code"],
        "toolset_disabled"
    );
    let refused = client
        .peer()
        .call_tool(CallToolRequestParams::new(SHOW_DOCUMENT).with_arguments(arguments()))
        .await
        .expect("a tool result, not a protocol error");
    assert_eq!(refused.is_error, Some(true));
    assert_eq!(
        refused.structured_content.expect("the refusal")["code"],
        "toolset_disabled"
    );

    client.cancel().await.expect("close MCP client");
    server_task.abort();
}
