// SPDX-License-Identifier: MPL-2.0

//! `/v1/runs/{run}/mcp` — the stable per-run MCP endpoint a delegated
//! provider is handed (ROADMAP §7.3, D6, D28).
//!
//! **Not the desktop bridge.** `koru-mcp` carries every local workspace and
//! twenty-odd document tools; this endpoint carries six artifact tools for
//! positively classified ordinary Runs. Bots use the engine's tool door.
//! Its authority comes from the bearer and private execution owner, never
//! provider-supplied identity. The only thing it knows about its
//! caller is the run authority record the bearer resolves to — never a run
//! id, an actor or a workspace the call names. Provenance derives from
//! that record; the grant set is read from it and frozen for the attempt;
//! a revocation or a fresh attempt is a row in `artifacts.db` that the
//! next call reads. The delegate owner must also confirm the current
//! generation on every request; an unavailable owner refuses tool access.
//!
//! **The transport is Streamable HTTP, stateless, JSON responses.** The
//! shape Codex's `mcp_servers.<name>.url` expects (`docs/features/terminal/providers/codex.md`),
//! and the shape the official SDK's own tower server offers under its
//! `json_response` setting: a JSON-RPC request is one `POST`, answered with
//! `application/json`; a notification is `202 Accepted`; there is no
//! `Mcp-Session-Id`, because there is no session — the bearer is the whole
//! of the caller's identity, resolved on every call — and `GET` answers
//! `405`, because the server never speaks first. The SDK's server-side HTTP
//! feature would have added `sse-stream` to the tree, which standing
//! requirement 12 refuses; what is reused instead is the SDK's *model* —
//! its JSON-RPC envelopes, its `Tool` definitions with schemas, its
//! `CallToolResult` with `structuredContent` — so a client built on the SDK
//! (the protocol test here uses one) sees exactly what it expects.
//!
//! The pieces: [`transport`] parses and answers JSON-RPC; [`tools`] is the
//! six ordinary definitions and their dispatch; [`staging`] is the register path
//! rule over `<run-workspace>/.artifacts/<RunId>/`; [`blobs`] is the bytes'
//! round trip through the Store.

use std::sync::{Arc, OnceLock};

use axum::Router;
use axum::routing::any;

use crate::GatewayState;

// **`pub(crate)` on two of the four, deliberately.** The register path
// rule and the bytes' round trip through the Store are what a *person's*
// Register must also obey (D19), and the artifacts routes call these two
// rather than growing a second copy of either — see
// `artifacts/run_folder.rs`. The transport and the tool definitions stay
// private: those are the run endpoint's, and nothing else has a run bearer.
pub(crate) mod blobs;
pub(crate) mod staging;
mod tools;
mod transport;

/// The one mount. `any`, because the transport answers `405` itself for
/// the methods it does not serve, with the code a client can read.
pub const ENDPOINT_PATH: &str = "/v1/runs/{run}/mcp";

pub fn router() -> Router<Arc<GatewayState>> {
    Router::new().route(
        ENDPOINT_PATH,
        any(transport::handle).layer(axum::extract::DefaultBodyLimit::max(64 * 1024)),
    )
}

/// Whether a request path is a run endpoint — the one place the gateway's
/// bearer admission steps aside, because the caller is a run and its
/// credential is resolved by [`transport::handle`], never by the session
/// authority.
pub fn is_run_endpoint(path: &str) -> bool {
    path.strip_prefix("/v1/runs/")
        .and_then(|rest| rest.strip_suffix("/mcp"))
        .is_some_and(|run| !run.is_empty() && !run.contains('/'))
}

/// `KORU_RUN_MCP_DEBUG`: one line per call with the tool's name, the code
/// it answered and the elapsed time — never an argument, a path or a
/// title. Free when off: one `OnceLock<bool>` and a branch.
pub(crate) fn debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| koru_env::switch(koru_env::name::KORU_RUN_MCP_DEBUG))
}

pub fn extend_openapi(document: &mut serde_json::Value) {
    use serde_json::{Map, Value, json};

    let components = document["components"]["schemas"]
        .as_object_mut()
        .expect("gateway OpenAPI schemas");
    // A tool's schema is JSON Schema 2020-12 with its named types under
    // `$defs`; the public document keeps every named type under
    // `components/schemas`, so they are hoisted there (the artifact routes'
    // copies win where a name is already present) and the references
    // rewritten.
    let tools: Vec<Value> = tools::definitions()
        .into_iter()
        .map(|tool| {
            let mut tool = serde_json::to_value(tool).expect("a tool definition serializes");
            for schema in ["inputSchema", "outputSchema"] {
                if let Some(defs) = tool[schema]
                    .as_object_mut()
                    .and_then(|object| object.remove("$defs"))
                    .and_then(|defs| defs.as_object().cloned())
                {
                    for (name, definition) in defs {
                        components.entry(name).or_insert(definition);
                    }
                }
            }
            rewrite_refs(&mut tool);
            tool
        })
        .collect();
    let operation = json!({
        "operationId": "runs_mcp",
        "summary": "A delegated run's MCP endpoint (Streamable HTTP, stateless)",
        "description": "The endpoint a delegated provider is handed. Authenticated by the Run bearer and positively classified by the private execution owner; unknown authority never falls back to ordinary tools. One JSON-RPC message per POST, answered as application/json; a notification is 202; GET and DELETE are 405. Ordinary Runs expose create, create_version, get, read, search, attach_to_run, with definitions in x-mcp-tools. Bots use their engine-owned tool endpoint. The request limit is 64 KiB.",
        "security": [{"runBearer": []}],
        "x-mcp-tools": tools,
        "requestBody": {
            "required": true,
            "content": {"application/json": {"schema": {"type": "object", "description": "One JSON-RPC 2.0 message: initialize, ping, tools/list, tools/call, or a notification."}}}
        },
        "responses": {
            "200": {"description": "The JSON-RPC response", "content": {"application/json": {"schema": {"type": "object"}}}},
            "202": {"description": "A notification was accepted; no body"},
            "400": {"description": "The body was not one JSON-RPC message", "content": {"application/json": {"schema": {"type": "object"}}}},
            "401": {"description": "The bearer is absent, unknown (authority_unknown), belongs to an earlier attempt (stale_generation), or the run's authority was revoked (authority_revoked)", "content": {"application/json": {"schema": {"type": "object"}}}},
            "405": {"description": "GET and DELETE: the endpoint is stateless and never speaks first"},
            "503": {"description": "The bearer authority or private Run classification is unavailable"}
        }
    });
    let paths = document["paths"]
        .as_object_mut()
        .expect("gateway OpenAPI paths");
    let entry = paths
        .entry(ENDPOINT_PATH.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    entry
        .as_object_mut()
        .expect("run endpoint OpenAPI path")
        .insert("post".into(), operation);
    let schemes = document["components"]["securitySchemes"]
        .as_object_mut()
        .expect("gateway OpenAPI security schemes");
    schemes.insert(
        "runBearer".into(),
        json!({"type": "http", "scheme": "bearer", "description": "A run's endpoint token, issued once per attempt by POST /v1/artifacts/authorities and rotated by POST /v1/artifacts/authorities/{run}/generation."}),
    );
}

/// `#/$defs/<name>` → `#/components/schemas/<name>`, everywhere in a value.
fn rewrite_refs(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(serde_json::Value::String(reference)) = object.get_mut("$ref")
                && let Some(name) = reference.strip_prefix("#/$defs/")
            {
                *reference = format!("#/components/schemas/{name}");
            }
            for member in object.values_mut() {
                rewrite_refs(member);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(rewrite_refs),
        _ => {}
    }
}

#[cfg(test)]
mod tests;
