// SPDX-License-Identifier: MPL-2.0
use super::*;
use rmcp::model::CallToolResult;
use serde_json::{Value, json};
const LIMIT: usize = 4 * 1024 * 1024;
fn id(text: &str) -> RequestId {
    serde_json::from_value(json!(text)).unwrap()
}
async fn body(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), LIMIT)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
#[test]
fn routine_frame_preflight_counts_exact_id_and_both_error_frames_before_mutation() {
    let fixed = serde_json::to_vec(&error_frame(id(""), true))
        .unwrap()
        .len();
    let fitting = id(&"x".repeat(LIMIT - fixed));
    assert!(preflight(&fitting).is_ok());
    let too_large = id(&"x".repeat(LIMIT - fixed + 1));
    assert_eq!(
        preflight(&too_large).err().unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    // Escaping, not scalar count, defines the encoded ID contribution.
    assert!(preflight(&id(&"\"".repeat(LIMIT / 2))).is_err());
}
#[tokio::test]
async fn routine_frame_keeps_arbitrary_id_and_counts_full_success_envelope() {
    let request_id = id("a\"b\\c\n🌳");
    let result = ServerResult::CallToolResult(CallToolResult::structured(json!({"ok":true})));
    let frame: ServerJsonRpcMessage = JsonRpcMessage::response(result.clone(), request_id.clone());
    let expected = serde_json::to_value(frame).unwrap();
    let response = answer(request_id, result, false);
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, expected);
}
#[tokio::test]
async fn routine_frame_oversized_accepted_mutation_returns_same_id_unknown_guidance() {
    let request_id = id("exact-request");
    let result = ServerResult::CallToolResult(CallToolResult::structured(
        json!({"large":"x".repeat(LIMIT)}),
    ));
    let value = body(answer(request_id, result, true)).await;
    assert_eq!(value["id"], "exact-request");
    assert_eq!(
        value["result"]["structuredContent"]["code"],
        "save_status_unknown"
    );
    assert!(value.to_string().contains("original request identity"));
}
#[tokio::test]
async fn routine_frame_oversized_read_is_distinct_and_minimum_failure_is_uncorrelated() {
    let value = body(answer(
        id("read"),
        ServerResult::CallToolResult(CallToolResult::structured(
            json!({"large":"x".repeat(LIMIT)}),
        )),
        false,
    ))
    .await;
    assert_eq!(value["id"], "read");
    assert_eq!(
        value["result"]["structuredContent"]["code"],
        "context_too_large"
    );
    let response = answer(
        id(&"x".repeat(LIMIT)),
        ServerResult::CallToolResult(CallToolResult::structured(json!({}))),
        true,
    );
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let value = body(response).await;
    assert!(value.get("id").is_none());
}
