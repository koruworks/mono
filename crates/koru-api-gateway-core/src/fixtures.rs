// SPDX-License-Identifier: MPL-2.0
//! Private-route composition only; not full GatewayState/bootstrap acceptance.
use koru_ai_server::AiWorkerClient;
use koru_delegate_api::routines::RoutineBridgeAuthority;

pub fn routine_http_router(
    authority: RoutineBridgeAuthority,
    worker: Option<AiWorkerClient>,
) -> axum::Router {
    crate::routine_bridge::fixture_router(authority, worker)
}
