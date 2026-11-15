// SPDX-License-Identifier: MPL-2.0
use super::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};

const LIMIT: usize = NATIVE_FRAME_BYTES;

fn id(text: &str) -> RequestId {
    serde_json::from_value(json!(text)).unwrap()
}
async fn body(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 2 * LIMIT)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
fn call(id: &str, tool: &str) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":tool,"arguments":{}}}))
        .unwrap(),
    )
}

#[tokio::test]
async fn native_mutation_giant_id_preflight_refuses_before_any_owner_call() {
    let fixed = serde_json::to_vec(&error_frame(id(""), "decision_status_unknown"))
        .unwrap()
        .len();
    // The request itself fits the Bot body limit; its unknown answer could not.
    let giant = "x".repeat(LIMIT - fixed + 1);
    for tool in ["edit_document", "create_document"] {
        let dispatched = AtomicUsize::new(0);
        let counter = &dispatched;
        let response = authenticated_message(true, call(&giant, tool), move |_| async move {
            let dispatched = counter;
            dispatched.fetch_add(1, Ordering::SeqCst);
            CallToolResult::structured(json!({}))
        })
        .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE, "{tool}");
        assert_eq!(
            dispatched.load(Ordering::SeqCst),
            0,
            "{tool}: no owner call"
        );
    }
    // One byte less fits: the exact encoded ID, both unknown answers counted.
    assert!(preflight(&id(&"x".repeat(LIMIT - fixed))).is_ok());
    // Escaping, not scalar count, defines the encoded ID contribution.
    assert!(preflight(&id(&"\"".repeat(LIMIT / 2))).is_err());
}

#[tokio::test]
async fn native_answer_keeps_the_arbitrary_id_and_the_exact_frame() {
    let request_id = id("a\"b\\c\n🌳");
    let result = ServerResult::CallToolResult(CallToolResult::structured_error(json!(
        {"code":"permission_pending","message":"m","details":{"operation_id":"op"}}
    )));
    let frame: ServerJsonRpcMessage = JsonRpcMessage::response(result.clone(), request_id.clone());
    let expected = serde_json::to_value(frame).unwrap();
    let response = answer(request_id, result);
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, expected);
}

#[tokio::test]
async fn native_postcommit_overflow_is_same_id_unknown_guidance() {
    // A decision answer keeps the decision's unknown code.
    let decision = ServerResult::CallToolResult(CallToolResult::structured_error(json!(
        {"code":"decision_in_progress","message":"x".repeat(LIMIT),"details":{}}
    )));
    let value = body(answer(id("exact-decision"), decision)).await;
    assert_eq!(value["id"], "exact-decision");
    assert_eq!(
        value["result"]["structuredContent"]["code"],
        "decision_status_unknown"
    );
    assert!(value.to_string().contains("original request identity"));
    // Any other mutation answer keeps Save's unknown code.
    let saved = ServerResult::CallToolResult(CallToolResult::structured(
        json!({"status":"saved","title":"x".repeat(LIMIT)}),
    ));
    let value = body(answer(id("exact-save"), saved)).await;
    assert_eq!(value["id"], "exact-save");
    assert_eq!(
        value["result"]["structuredContent"]["code"],
        "save_status_unknown"
    );
}
