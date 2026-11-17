// SPDX-License-Identifier: MPL-2.0

//! The framing on its own: what goes out, and what each kind of response is
//! read as.

use serde_json::{Value, json};

use super::*;
use crate::transport::InvalidAccessToken;

const TOKEN: &str = "eyJ.fake-bearer.value";

fn token() -> AccessToken {
    AccessToken::new(TOKEN).unwrap()
}

fn response(status: u16, body: Value) -> PostResponse {
    PostResponse {
        status,
        content_type: Some("application/json".into()),
        www_authenticate: None,
        body: serde_json::to_vec(&body).unwrap(),
    }
}

fn rpc_error_body(id: Option<u64>, code: i64, data: Value) -> Value {
    let mut message = json!({ "jsonrpc": "2.0", "error": { "code": code, "message": "Refused." } });
    if let Some(id) = id {
        message["id"] = json!(id);
    }
    if !data.is_null() {
        message["error"]["data"] = data;
    }
    message
}

#[track_caller]
fn refused(reply: Reply) -> Refusal {
    match reply {
        Reply::Refused(refusal) => refusal,
        Reply::Result(_) => panic!("expected a refusal, got a result"),
        Reply::NotUnderstood => panic!("expected a refusal, got nothing understood"),
    }
}

#[test]
fn a_tool_call_carries_the_revisions_headers_and_the_same_values_in_its_body() {
    let request = tools_call(7, "edit_document", &json!({ "document_id": "d" }), token());
    let headers = request.headers();
    let header = |name: &str| {
        headers
            .iter()
            .find(|(header, _)| *header == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(header("Content-Type"), Some("application/json"));
    assert_eq!(
        header("Accept"),
        Some("application/json, text/event-stream")
    );
    assert_eq!(header("MCP-Protocol-Version"), Some("2026-07-28"));
    assert_eq!(header("Mcp-Method"), Some("tools/call"));
    assert_eq!(header("Mcp-Name"), Some("edit_document"));
    assert_eq!(
        header("Authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );

    let body: Value = serde_json::from_slice(request.body()).unwrap();
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 7);
    assert_eq!(body["method"], "tools/call");
    assert_eq!(body["params"]["name"], "edit_document");
    assert_eq!(body["params"]["arguments"], json!({ "document_id": "d" }));
    let meta = &body["params"]["_meta"];
    assert_eq!(
        meta["io.modelcontextprotocol/protocolVersion"],
        PROTOCOL_REVISION
    );
    assert!(meta["io.modelcontextprotocol/clientCapabilities"].is_object());

    // The list names no tool, so it carries no name header.
    let request = tools_list(8, Some("4"), token());
    assert!(
        request
            .headers()
            .iter()
            .all(|(header, _)| *header != "Mcp-Name")
    );
    let body: Value = serde_json::from_slice(request.body()).unwrap();
    assert_eq!(body["method"], "tools/list");
    assert_eq!(body["params"]["cursor"], "4");
}

#[test]
fn a_request_and_a_token_print_neither_the_bearer_nor_the_body() {
    let request = tools_call(
        1,
        "edit_document",
        &json!({ "text": "Private-Words" }),
        token(),
    );
    let printed = format!("{request:?} {:?}", token());
    assert!(
        !printed.contains(TOKEN) && !printed.contains("Bearer"),
        "{printed}"
    );
    assert!(!printed.contains("Private-Words"), "{printed}");
    assert!(printed.contains("tools/call") && printed.contains("edit_document"));

    let reply = response(
        200,
        json!({ "jsonrpc": "2.0", "id": 1, "result": { "text": "Private-Words" } }),
    );
    assert!(!format!("{reply:?}").contains("Private-Words"));
}

#[test]
fn a_token_that_could_add_a_header_is_not_a_token() {
    for bad in [
        "",
        "two words",
        "line\r\nX-Other: 1",
        "tab\there",
        "caf\u{e9}",
    ] {
        assert_eq!(
            AccessToken::new(bad).err(),
            Some(InvalidAccessToken),
            "{bad:?}"
        );
    }
    assert!(AccessToken::new("abc.DEF-123_~+/=").is_ok());
}

#[test]
fn a_401_is_token_expired_whatever_else_it_says() {
    let mut challenge = response(401, json!({ "jsonrpc": "2.0", "id": 1, "result": {} }));
    challenge.www_authenticate =
        Some("Bearer resource_metadata=\"https://fake.invalid/meta\"".into());
    assert_eq!(refused(read_reply(1, &challenge)), Refusal::TokenExpired);
    let bare = PostResponse {
        status: 401,
        content_type: None,
        www_authenticate: None,
        body: Vec::new(),
    };
    assert_eq!(refused(read_reply(1, &bare)), Refusal::TokenExpired);
}

#[test]
fn a_403_is_insufficient_scope_only_when_it_says_so() {
    let mut by_header = response(403, json!({}));
    by_header.www_authenticate = Some("Bearer error=\"insufficient_scope\"".into());
    assert_eq!(
        refused(read_reply(1, &by_header)),
        Refusal::InsufficientScope
    );

    let by_body = response(403, json!({ "error": "insufficient_scope" }));
    assert_eq!(refused(read_reply(1, &by_body)), Refusal::InsufficientScope);
    let by_data = response(
        403,
        rpc_error_body(Some(1), -32000, json!({ "code": "insufficient_scope" })),
    );
    assert_eq!(refused(read_reply(1, &by_data)), Refusal::InsufficientScope);

    let other = PostResponse {
        status: 403,
        content_type: None,
        www_authenticate: None,
        body: b"Forbidden".to_vec(),
    };
    assert_eq!(refused(read_reply(1, &other)), Refusal::Forbidden);

    // Admission's own codes keep their meaning under a 403.
    let revoked = response(
        403,
        rpc_error_body(Some(1), -32000, json!({ "code": "authorization_revoked" })),
    );
    assert_eq!(refused(read_reply(1, &revoked)), Refusal::ConnectionRevoked);
    let ended = response(
        200,
        rpc_error_body(
            Some(1),
            -32000,
            json!({ "code": "cloud_subscription_ended" }),
        ),
    );
    assert_eq!(refused(read_reply(1, &ended)), Refusal::SubscriptionEnded);
}

#[test]
fn a_refused_revision_lists_only_what_looks_like_a_revision() {
    let refusal = response(
        400,
        rpc_error_body(
            Some(1),
            -32022,
            json!({
                "requested": "2026-07-28",
                "supported": ["2025-11-25", "not a revision", 20250618, "2025-06-18"],
            }),
        ),
    );
    assert_eq!(
        refused(read_reply(1, &refusal)),
        Refusal::RevisionRefused {
            supported: vec!["2025-11-25".to_owned(), "2025-06-18".to_owned()],
        }
    );
    // With nothing listed it is still the same refusal, and it may leave the
    // id out.
    let bare = response(400, rpc_error_body(None, -32022, Value::Null));
    assert_eq!(
        refused(read_reply(1, &bare)),
        Refusal::RevisionRefused {
            supported: Vec::new()
        }
    );
}

#[test]
fn the_front_doors_other_refusals_prove_nothing_ran_and_no_other_error_does() {
    for (status, code) in [(400, -32020), (404, -32601)] {
        let refusal = response(status, rpc_error_body(Some(1), code, Value::Null));
        assert_eq!(
            refused(read_reply(1, &refusal)),
            Refusal::RequestRefused { rpc_code: code }
        );
    }
    // An internal error, invalid params, a parse error: any of them could
    // follow a tool that had begun.
    for (status, code) in [
        (500, -32603),
        (200, -32603),
        (400, -32602),
        (400, -32700),
        (200, -32000),
    ] {
        let other = response(status, rpc_error_body(Some(1), code, Value::Null));
        assert!(
            matches!(read_reply(1, &other), Reply::NotUnderstood),
            "{status} {code}"
        );
    }
}

#[test]
fn only_a_200_result_for_this_request_is_a_result() {
    let result = json!({ "structuredContent": { "revision": "0a" } });
    let message = |id: Value| json!({ "jsonrpc": "2.0", "id": id, "result": result });

    assert!(matches!(
        read_reply(5, &response(200, message(json!(5)))),
        Reply::Result(_)
    ));
    // Another request's answer, an id that is not this one's, none at all.
    for id in [json!(6), json!("5"), Value::Null] {
        assert!(
            matches!(
                read_reply(5, &response(200, message(id.clone()))),
                Reply::NotUnderstood
            ),
            "{id}"
        );
    }
    // A result under a status that is not success.
    for status in [202, 500, 502] {
        assert!(matches!(
            read_reply(5, &response(status, message(json!(5)))),
            Reply::NotUnderstood
        ));
    }
    // A body that is not a message.
    for body in ["", "<html>Bad gateway</html>", "[1,2]", "\"text\""] {
        let reply = PostResponse {
            status: 200,
            content_type: Some("application/json".into()),
            www_authenticate: None,
            body: body.as_bytes().to_vec(),
        };
        assert!(matches!(read_reply(5, &reply), Reply::NotUnderstood));
    }
    // A result that is not an object.
    let scalar = json!({ "jsonrpc": "2.0", "id": 5, "result": "done" });
    assert!(matches!(
        read_reply(5, &response(200, scalar)),
        Reply::NotUnderstood
    ));
}

#[test]
fn an_event_stream_is_read_for_its_first_message() {
    let message = json!({ "jsonrpc": "2.0", "id": 3, "result": { "structuredContent": {} } });
    let stream = |text: String| PostResponse {
        status: 200,
        content_type: Some("text/event-stream; charset=utf-8".into()),
        www_authenticate: None,
        body: text.into_bytes(),
    };
    let plain = format!("event: message\ndata: {message}\n\n");
    assert!(matches!(read_reply(3, &stream(plain)), Reply::Result(_)));
    // No blank line after the last event, carriage returns, a comment and an
    // empty event first, data over two lines.
    let pretty = serde_json::to_string_pretty(&message).unwrap();
    let split = pretty
        .lines()
        .map(|line| format!("data: {line}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let untidy = format!(": keep-alive\r\n\r\nevent: message\r\n{split}");
    assert!(matches!(read_reply(3, &stream(untidy)), Reply::Result(_)));
    // A stream with no message in it.
    assert!(matches!(
        read_reply(3, &stream(": keep-alive\n\n".into())),
        Reply::NotUnderstood
    ));
    assert!(matches!(
        read_reply(3, &stream("data: not json\n\n".into())),
        Reply::NotUnderstood
    ));
}

#[test]
fn a_tool_answer_is_a_success_or_a_failure_only_with_an_object() {
    let answer = |result: Value| tool_answer(Reply::Result(result));
    assert!(matches!(
        answer(json!({ "structuredContent": { "revision": "0a" }, "resultType": "complete" })),
        ToolAnswer::Success(_)
    ));
    assert!(matches!(
        answer(json!({ "structuredContent": { "revision": "0a" }, "isError": false })),
        ToolAnswer::Success(_)
    ));
    assert!(matches!(
        answer(json!({ "structuredContent": { "code": "revision_conflict" }, "isError": true })),
        ToolAnswer::Failure(_)
    ));
    for not_an_answer in [
        // No structured content, or one that is not an object.
        json!({ "content": [{ "type": "text", "text": "{\"revision\":\"0a\"}" }] }),
        json!({ "structuredContent": [1, 2] }),
        // A result that is not complete.
        json!({ "structuredContent": {}, "resultType": "input_required" }),
        // An error flag that is not a flag.
        json!({ "structuredContent": {}, "isError": "no" }),
    ] {
        assert!(
            matches!(answer(not_an_answer.clone()), ToolAnswer::NotUnderstood),
            "{not_an_answer}"
        );
    }
    assert!(matches!(
        tool_answer(Reply::Refused(Refusal::TokenExpired)),
        ToolAnswer::Refused(Refusal::TokenExpired)
    ));
    assert!(matches!(
        tool_answer(Reply::NotUnderstood),
        ToolAnswer::NotUnderstood
    ));
}

#[test]
fn request_ids_do_not_repeat() {
    let ids = Ids::new();
    let (first, second) = (ids.next(), ids.next());
    assert_ne!(first, second);
}
