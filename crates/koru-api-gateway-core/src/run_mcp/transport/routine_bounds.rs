// SPDX-License-Identifier: MPL-2.0
//! Count the complete SDK frame, including the unmodified arbitrary request ID.
use super::*;
use koru_ai_core::bots::validate_routine_list_bytes;

fn overflow() -> Response {
    refuse(
        StatusCode::PAYLOAD_TOO_LARGE,
        native::routines::failure("context_too_large"),
    )
}
fn error_frame(id: RequestId, mutation: bool) -> ServerJsonRpcMessage {
    JsonRpcMessage::response(
        ServerResult::CallToolResult(native::routines::error(if mutation {
            "save_status_unknown"
        } else {
            "context_too_large"
        })),
        id,
    )
}
pub(super) fn preflight(id: &RequestId) -> Result<(), Response> {
    for mutation in [false, true] {
        validate_routine_list_bytes(&error_frame(id.clone(), mutation)).map_err(|_| overflow())?;
    }
    Ok(())
}
pub(super) fn answer(id: RequestId, result: ServerResult, mutation: bool) -> Response {
    let frame: ServerJsonRpcMessage = JsonRpcMessage::response(result, id.clone());
    if validate_routine_list_bytes(&frame).is_ok() {
        return Json(frame).into_response();
    }
    // Acceptance may already have occurred. Never turn an oversized mutation
    // result into an instruction to mint a different request/candidate.
    let fallback = error_frame(id, mutation);
    if validate_routine_list_bytes(&fallback).is_ok() {
        Json(fallback).into_response()
    } else {
        overflow()
    }
}
#[cfg(test)]
mod tests;
