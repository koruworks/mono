// SPDX-License-Identifier: MPL-2.0
//! Mounted production body matcher after a fixed vouched classification. This
//! fixture does not qualify artifact bearer auth or full GatewayState startup.
use super::*;
use axum::{Router, body::Body, http::Request, routing::post};
use rmcp::model::CallToolResult;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;
const LIMIT: usize = 4 * 1024 * 1024;
async fn send(
    id: Value,
    name: &str,
    bot: bool,
    huge_result: bool,
    calls: Arc<AtomicUsize>,
) -> (StatusCode, Value) {
    let router = Router::new()
        .route(
            "/run",
            post(move |body: Bytes| {
                let calls = calls.clone();
                async move {
                    authenticated_message(bot, body, move |params| async move {
                        assert_eq!(params.name.as_ref(), "create_routine");
                        calls.fetch_add(1, Ordering::AcqRel);
                        CallToolResult::structured(if huge_result {
                            json!({"accepted":true,"value":"x".repeat(LIMIT)})
                        } else {
                            json!({"accepted":true})
                        })
                    })
                    .await
                }
            }),
        )
        .layer(axum::extract::DefaultBodyLimit::disable());
    let bytes = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":{}}})).unwrap();
    let response = router
        .oneshot(
            Request::post("/run")
                .header("content-type", "application/json")
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), LIMIT)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn routine_transport_giant_id_refuses_before_any_mutation_dispatch() {
    let calls = Arc::new(AtomicUsize::new(0));
    // Request fits4MiB; the exact ID plus required unknown-result frame does not.
    let id = "x".repeat(LIMIT - 256);
    let (status, value) = send(json!(id), "create_routine", true, false, calls.clone()).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(calls.load(Ordering::Acquire), 0);
    assert!(value.get("id").is_none());
}
#[tokio::test]
async fn routine_transport_accepted_overflow_is_one_dispatch_and_exact_unknown_id() {
    let calls = Arc::new(AtomicUsize::new(0));
    let id = "request\"\\🌳";
    let (status, value) = send(json!(id), "create_routine", true, true, calls.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert_eq!(value["id"], id);
    assert_eq!(
        value["result"]["structuredContent"]["code"],
        "save_status_unknown"
    );
    assert!(value.to_string().contains("original request identity"));
}
#[tokio::test]
async fn routine_transport_keeps_ordinary_input_limit_and_bot_full_request_limit() {
    for (bot, len) in [(false, 65536), (true, LIMIT)] {
        let calls = Arc::new(AtomicUsize::new(0));
        let (status, _) = send(
            json!("x".repeat(len)),
            "create_routine",
            bot,
            false,
            calls.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(calls.load(Ordering::Acquire), 0);
    }
}
