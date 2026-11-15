// SPDX-License-Identifier: MPL-2.0
//! The complete frame of a native Bot mutation (`create_document`,
//! `edit_document`), counted with the unmodified arbitrary JSON-RPC request
//! ID (plan M4 P4.2 F; contract section 9). The preflight runs before any
//! owner call, so an ID whose own unknown-outcome answer could not be framed
//! is refused while nothing can have been accepted. After acceptance an
//! oversized answer becomes same-ID unknown guidance, never an instruction to
//! mint another operation.
use super::*;
use rmcp::model::CallToolResult;
use serde_json::json;

/// A native mutation's answer must fit the same frame its request may use.
pub(super) const NATIVE_FRAME_BYTES: usize = 4 * 1024 * 1024;

pub(super) fn recognizes(name: &str) -> bool {
    matches!(name, "create_document" | "edit_document")
}

fn unknown(code: &'static str) -> CallToolResult {
    CallToolResult::structured_error(json!(ToolFailure::new(
        code,
        "The outcome could not be returned in one answer. Keep the original request identity: retry with the same operation_id and identical arguments, or read the page before continuing.",
    )))
}

fn error_frame(id: RequestId, code: &'static str) -> ServerJsonRpcMessage {
    JsonRpcMessage::response(ServerResult::CallToolResult(unknown(code)), id)
}

/// Whether the serialized frame fits, counted without allocating it.
fn fits(frame: &ServerJsonRpcMessage) -> bool {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|size| *size <= NATIVE_FRAME_BYTES)
                .ok_or_else(|| std::io::Error::other("native frame exceeded"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(0), frame).is_ok()
}

fn overflow() -> Response {
    refuse(
        StatusCode::PAYLOAD_TOO_LARGE,
        ToolFailure::new(
            "context_too_large",
            "The request ID is too large for the Run endpoint to answer.",
        ),
    )
}

/// Both unknown-outcome answers must fit with this exact ID before mutation.
pub(super) fn preflight(id: &RequestId) -> Result<(), Response> {
    for code in ["save_status_unknown", "decision_status_unknown"] {
        if !fits(&error_frame(id.clone(), code)) {
            return Err(overflow());
        }
    }
    Ok(())
}

/// A decision answer (pending, unknown, in progress or closed) keeps the
/// decision's own unknown code; any other mutation answer keeps Save's.
fn decision_answer(result: &ServerResult) -> bool {
    let ServerResult::CallToolResult(result) = result else {
        return false;
    };
    result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("code"))
        .and_then(|code| code.as_str())
        .is_some_and(|code| code == "permission_pending" || code.starts_with("decision_"))
}

pub(super) fn answer(id: RequestId, result: ServerResult) -> Response {
    let decision = decision_answer(&result);
    let frame: ServerJsonRpcMessage = JsonRpcMessage::response(result, id.clone());
    if fits(&frame) {
        return Json(frame).into_response();
    }
    let fallback = error_frame(
        id,
        if decision {
            "decision_status_unknown"
        } else {
            "save_status_unknown"
        },
    );
    if fits(&fallback) {
        Json(fallback).into_response()
    } else {
        overflow()
    }
}

#[cfg(test)]
mod tests;
