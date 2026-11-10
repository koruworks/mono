// SPDX-License-Identifier: MPL-2.0

//! The run endpoint in process: a real Store worker and a real
//! `artifacts.db` under one temporary root, a run workspace with an
//! `.artifacts/<RunId>/` folder under another, an authority issued through
//! the route, and the endpoint driven the way a provider drives it — raw
//! JSON-RPC over `POST`, and once through the official SDK's client over a
//! transport of its own. Every refusal §7.3 names is a case here with its
//! stable code, and the two restarts the roadmap asks about are asserted
//! with no delegate process at all: the authority is in the gateway's
//! database, so nothing else has to be alive.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use koru_artifact_core::{AttemptGeneration, RunId, RunWorkspace};
use koru_artifact_wire::run_tools::{READ_BOUND_BYTES, TOOL_NAMES};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::staging::{self, StageError};
use crate::{GatewayState, now_millis, router};

// ------------------------------------------------------------- the fixture

fn gateway(root: &Path) -> Arc<GatewayState> {
    let worker = koru_store_server::StoreWorker::new(koru_store_server::WorkerConfig::embedded(
        root.to_path_buf(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let state = GatewayState::new("internal-debug".into(), worker);
    struct OrdinaryOwner;
    impl koru_delegate_api::bots::BotDelegatePort for OrdinaryOwner {
        fn execute(
            &self,
            command: koru_delegate_api::bots::BotCommand,
        ) -> Result<koru_delegate_api::bots::BotReply, koru_delegate_api::Failure> {
            assert!(matches!(
                command,
                koru_delegate_api::bots::BotCommand::ResolveRunAuthority { .. }
            ));
            Ok(koru_delegate_api::bots::BotReply::RunAuthority {
                authority: koru_delegate_core::RunToolAuthority::Ordinary {},
            })
        }
    }
    assert!(state.set_bot_delegate_port(Arc::new(OrdinaryOwner)));
    let repository =
        koru_artifact_sqlite::Repository::open(&koru_channel::artifacts_root_beside(root)).unwrap();
    assert!(state.set_artifact_service(Arc::new(koru_artifact_api::Service::new(repository))));
    state
}

struct Fixture {
    _root: tempfile::TempDir,
    state: Arc<GatewayState>,
    bearer: String,
    /// The run's working scope on disk, with `.artifacts/<run>/` made.
    run_workspace: tempfile::TempDir,
    run: RunId,
    workspace: String,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = gateway(root.path());
        let (bearer, _) = state.sessions.mint("test", &[], now_millis());
        let run_workspace = tempfile::tempdir().unwrap();
        let run = RunId::mint();
        std::fs::create_dir_all(koru_artifact_core::artifacts_folder(
            run_workspace.path(),
            run,
        ))
        .unwrap();
        let workspace = make_workspace(&state, &bearer, "Notes").await;
        Self {
            _root: root,
            state,
            bearer,
            run_workspace,
            run,
            workspace,
        }
    }

    fn folder(&self) -> PathBuf {
        koru_artifact_core::artifacts_folder(self.run_workspace.path(), self.run)
    }

    fn write(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.folder().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// Issue `run`'s authority with `grants`, through the route.
    async fn issue(&self, run: RunId, grants: &[&str]) -> Value {
        let (status, issued) = json(
            &self.state,
            &self.bearer,
            "POST",
            "/v1/artifacts/authorities",
            Some(json!({
                "run": run.to_storage(),
                "workspace": self.workspace,
                "generation": 1,
                "grants": grants,
                "run_workspace": RunWorkspace::new(self.run_workspace.path()).unwrap(),
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{issued}");
        issued
    }

    async fn token(&self, grants: &[&str]) -> String {
        self.issue(self.run, grants).await["token"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn rpc(&self, token: &str, method: &str, params: Value) -> (StatusCode, Value) {
        rpc(&self.state, self.run, token, method, params).await
    }

    /// `tools/call`, answering `(isError, structuredContent)`.
    async fn call(&self, token: &str, tool: &str, arguments: Value) -> (bool, Value) {
        let (status, response) = self
            .rpc(
                token,
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        tool_output(&response)
    }

    async fn refused(&self, token: &str, tool: &str, arguments: Value) -> String {
        let (is_error, output) = self.call(token, tool, arguments).await;
        assert!(is_error, "{output}");
        output["code"].as_str().unwrap().to_owned()
    }
}

async fn send(
    state: &Arc<GatewayState>,
    bearer: Option<&str>,
    method: &str,
    uri: &str,
    body: Option<Vec<u8>>,
) -> (StatusCode, header::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:4242");
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let body = match body {
        Some(bytes) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(bytes)
        }
        None => Body::empty(),
    };
    let response = router(Arc::clone(state))
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, headers, bytes.to_vec())
}

async fn json(
    state: &Arc<GatewayState>,
    bearer: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let body = body.map(|value| serde_json::to_vec(&value).unwrap());
    let (status, _, bytes) = send(state, Some(bearer), method, uri, body).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn make_workspace(state: &Arc<GatewayState>, bearer: &str, name: &str) -> String {
    let (status, profile) = json(
        state,
        bearer,
        "POST",
        "/v1/profiles",
        Some(json!({ "name": "Ada", "locale": "en" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{profile}");
    let (status, workspace) = json(
        state,
        bearer,
        "POST",
        "/v1/workspaces",
        Some(json!({ "owner": profile["id"], "name": name })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{workspace}");
    workspace["id"].as_str().unwrap().to_owned()
}

/// A family a person made by hand in `workspace`, with one version.
async fn family_by_hand(
    state: &Arc<GatewayState>,
    bearer: &str,
    workspace: &str,
) -> (String, String) {
    let (status, _, blob) = send(
        state,
        Some(bearer),
        "PUT",
        &format!("/v1/workspaces/{workspace}/blobs"),
        Some(b"by hand".to_vec()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let blob: Value = serde_json::from_slice(&blob).unwrap();
    let family_id = uuid::Uuid::new_v4().to_string();
    let (status, family) = json(
        state,
        bearer,
        "POST",
        "/v1/artifacts/families",
        Some(
            json!({ "family_id": family_id, "workspace": workspace, "title": "Quarterly report" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{family}");
    let (status, registered) = json(
        state,
        bearer,
        "POST",
        &format!("/v1/artifacts/families/{family_id}/versions"),
        Some(json!({
            "registration_id": uuid::Uuid::new_v4().to_string(),
            "blob": blob["hash"],
            "mime": "text/plain",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    (
        family_id,
        registered["version"]["id"].as_str().unwrap().to_owned(),
    )
}

async fn rpc(
    state: &Arc<GatewayState>,
    run: RunId,
    token: &str,
    method: &str,
    params: Value,
) -> (StatusCode, Value) {
    let message = json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params });
    let (status, _, bytes) = send(
        state,
        Some(token),
        "POST",
        &format!("/v1/runs/{}/mcp", run.to_storage()),
        Some(serde_json::to_vec(&message).unwrap()),
    )
    .await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn tool_output(response: &Value) -> (bool, Value) {
    let result = &response["result"];
    assert!(result.is_object(), "not a tool result: {response}");
    (
        result["isError"].as_bool().unwrap_or(false),
        result["structuredContent"].clone(),
    )
}

fn initialize_params() -> Value {
    json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": { "name": "test", "version": "0" }
    })
}

// ------------------------------------------------------------- the transport

#[tokio::test]
async fn the_endpoint_speaks_stateless_streamable_http_and_admits_only_a_run_token() {
    let fixture = Fixture::new().await;
    let token = fixture.token(&[]).await;

    // initialize: tools capability, no session id, the client's version.
    let (status, response) = fixture.rpc(&token, "initialize", initialize_params()).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["id"], 7);
    assert_eq!(response["result"]["protocolVersion"], "2025-06-18");
    assert!(response["result"]["capabilities"]["tools"].is_object());
    assert_eq!(response["result"]["serverInfo"]["name"], "koru-run");
    let uri = format!("/v1/runs/{}/mcp", fixture.run.to_storage());
    let (status, headers, _) = send(
        &fixture.state,
        Some(&token),
        "POST",
        &uri,
        Some(
            serde_json::to_vec(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .unwrap(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "a notification is accepted");
    assert!(headers.get("mcp-session-id").is_none(), "stateless");

    // tools/list: exactly the six, in order, each with both schemas.
    let (status, response) = fixture.rpc(&token, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let tools = response["result"]["tools"].as_array().unwrap();
    let names: Vec<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, TOOL_NAMES);
    for tool in tools {
        assert!(tool["inputSchema"].is_object(), "{tool}");
        assert!(tool["outputSchema"].is_object(), "{tool}");
        assert!(tool["annotations"].is_object(), "{tool}");
    }
    // ping, and a method this endpoint does not serve.
    let (status, response) = fixture.rpc(&token, "ping", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert!(response["result"].is_object());
    let (status, response) = fixture.rpc(&token, "resources/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["error"]["code"], -32601);

    // GET and DELETE: 405, with the code.
    for method in ["GET", "DELETE"] {
        let (status, headers, body) = send(&fixture.state, Some(&token), method, &uri, None).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
        assert_eq!(headers.get(header::ALLOW).unwrap(), "POST");
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["code"], "method_not_allowed");
    }
    // Not JSON-RPC: a parse error with no id.
    let (status, _, body) = send(
        &fixture.state,
        Some(&token),
        "POST",
        &uri,
        Some(b"{".to_vec()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], -32700);

    // The gateway's own bearer is not a run token: unknown, before any
    // body is read. Nor is nothing.
    let (status, _, body) = send(
        &fixture.state,
        Some(&fixture.bearer),
        "POST",
        &uri,
        Some(b"{}".to_vec()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["code"], "authority_unknown");
    let (status, _, _) = send(&fixture.state, None, "POST", &uri, Some(b"{}".to_vec())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // And a run token is not a gateway bearer: every ordinary route
    // refuses it.
    let (status, _, _) = send(&fixture.state, Some(&token), "GET", "/v1/status", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(
        &fixture.state,
        Some(&token),
        "GET",
        &format!("/v1/artifacts/authorities/{}", fixture.run.to_storage()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A path that is not a run id.
    let (status, _, _) = send(
        &fixture.state,
        Some(&token),
        "POST",
        "/v1/runs/not-a-run/mcp",
        Some(b"{}".to_vec()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(super::is_run_endpoint("/v1/runs/x/mcp"));
    assert!(!super::is_run_endpoint("/v1/runs/x/mcp/more"));
    assert!(!super::is_run_endpoint("/v1/runs"));
    assert!(!super::is_run_endpoint("/v1/runs//mcp"));
}

/// The SDK's client, over a transport that carries each message to the
/// in-process router.
mod sdk_client {
    use std::sync::Arc;

    use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
    use rmcp::service::RoleClient;
    use rmcp::transport::Transport;
    use tokio::sync::mpsc;

    use crate::GatewayState;

    pub struct InProcess {
        state: Arc<GatewayState>,
        uri: String,
        token: String,
        inbox: mpsc::UnboundedReceiver<ServerJsonRpcMessage>,
        outbox: mpsc::UnboundedSender<ServerJsonRpcMessage>,
    }

    impl InProcess {
        pub fn new(state: Arc<GatewayState>, run: koru_artifact_core::RunId, token: &str) -> Self {
            let (outbox, inbox) = mpsc::unbounded_channel();
            Self {
                state,
                uri: format!("/v1/runs/{}/mcp", run.to_storage()),
                token: token.to_owned(),
                inbox,
                outbox,
            }
        }
    }

    impl Transport<RoleClient> for InProcess {
        type Error = std::io::Error;

        fn send(
            &mut self,
            item: ClientJsonRpcMessage,
        ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
            let state = Arc::clone(&self.state);
            let uri = self.uri.clone();
            let token = self.token.clone();
            let outbox = self.outbox.clone();
            async move {
                let body = serde_json::to_vec(&item).map_err(std::io::Error::other)?;
                let (status, _, bytes) =
                    super::send(&state, Some(&token), "POST", &uri, Some(body)).await;
                if status == axum::http::StatusCode::ACCEPTED {
                    return Ok(());
                }
                if !status.is_success() {
                    return Err(std::io::Error::other(format!("HTTP {status}")));
                }
                let message: ServerJsonRpcMessage =
                    serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
                outbox.send(message).map_err(std::io::Error::other)
            }
        }

        fn receive(&mut self) -> impl Future<Output = Option<ServerJsonRpcMessage>> + Send {
            self.inbox.recv()
        }

        fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
            std::future::ready(Ok(()))
        }
    }
}

/// Initialize, list the six, `create` a fixture file, `get` it back,
/// `search` finds it — through the official SDK's client.
#[tokio::test]
async fn the_sdks_client_initializes_lists_six_tools_creates_gets_and_searches() {
    use rmcp::model::CallToolRequestParams;

    let fixture = Fixture::new().await;
    let workspace = fixture.workspace.clone();
    let token = fixture.token(&[&workspace]).await;
    fixture.write("summary.md", b"# Summary\n\nDone.\n");

    let transport = sdk_client::InProcess::new(Arc::clone(&fixture.state), fixture.run, &token);
    let client = rmcp::service::serve_client((), transport).await.unwrap();
    let info = client.peer_info().expect("initialize answered");
    assert_eq!(info.server_info.as_ref().unwrap().name, "koru-run");
    assert!(info.capabilities.tools.is_some());
    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(names, TOOL_NAMES);

    let arguments = |value: Value| value.as_object().cloned().unwrap();
    let created = client
        .call_tool(
            CallToolRequestParams::new("create").with_arguments(arguments(
                json!({ "path": "summary.md", "title": "Summary", "mime": "text/markdown" }),
            )),
        )
        .await
        .unwrap();
    assert_ne!(created.is_error, Some(true), "{created:?}");
    let created = created.structured_content.unwrap();
    assert_eq!(created["retried"], false);
    assert_eq!(created["family"]["title"], "Summary");
    assert_eq!(created["family"]["workspace"], workspace);
    assert_eq!(created["version"]["ordinal"], 1);
    assert_eq!(created["version"]["mime"], "text/markdown");
    assert_eq!(created["version"]["filename"], "summary.md");
    assert_eq!(
        created["version"]["produced_by"]["run"],
        fixture.run.to_storage()
    );
    let version = created["version"]["id"].as_str().unwrap().to_owned();

    let got = client
        .call_tool(
            CallToolRequestParams::new("get")
                .with_arguments(arguments(json!({ "version": version }))),
        )
        .await
        .unwrap();
    assert_ne!(got.is_error, Some(true));
    assert_eq!(
        got.structured_content.unwrap()["version"],
        created["version"]
    );

    let found = client
        .call_tool(
            CallToolRequestParams::new("search")
                .with_arguments(arguments(json!({ "query": "summ" }))),
        )
        .await
        .unwrap();
    assert_ne!(found.is_error, Some(true));
    let found = found.structured_content.unwrap();
    assert_eq!(found["hits"].as_array().unwrap().len(), 1);
    assert_eq!(found["hits"][0]["family"]["id"], created["family"]["id"]);
    assert_eq!(found["hits"][0]["versions"][0]["id"], version);
    assert_eq!(found["truncated"], false);

    // A refusal is a structured error the model can read, not a
    // transport failure.
    let refused = client
        .call_tool(
            CallToolRequestParams::new("create").with_arguments(arguments(
                json!({ "path": "missing.md", "title": "Missing" }),
            )),
        )
        .await
        .unwrap();
    assert_eq!(refused.is_error, Some(true));
    assert_eq!(
        refused.structured_content.unwrap()["code"],
        "file_not_found"
    );
    client.cancel().await.unwrap();
}

// -------------------------------------------------------------- the tools

#[tokio::test]
async fn create_reads_the_file_under_the_run_folder_and_read_returns_it_bounded() {
    let fixture = Fixture::new().await;
    let token = fixture.token(&[]).await;
    let text = "# Findings\n\nRésumé — done.\n";
    fixture.write("report/findings.md", text.as_bytes());
    let (is_error, created) = fixture
        .call(
            &token,
            "create",
            json!({ "path": "report/findings.md", "title": "Findings" }),
        )
        .await;
    assert!(!is_error, "{created}");
    let version = created["version"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["version"]["size_bytes"], text.len());
    assert_eq!(created["version"]["mime"], "application/octet-stream");

    // The same file again: the same version. An absolute path inside the
    // folder is the same file too.
    let (_, again) = fixture
        .call(
            &token,
            "create",
            json!({ "path": "report/findings.md", "title": "Findings" }),
        )
        .await;
    assert_eq!(again["retried"], true);
    assert_eq!(again["version"]["id"], version);
    let absolute = fixture.folder().join("report").join("findings.md");
    let (_, absolute) = fixture
        .call(
            &token,
            "create",
            json!({ "path": absolute.to_str().unwrap(), "title": "Findings" }),
        )
        .await;
    assert_eq!(absolute["retried"], true);
    assert_eq!(absolute["version"]["id"], version);

    let (is_error, content) = fixture
        .call(&token, "read", json!({ "version": version }))
        .await;
    assert!(!is_error, "{content}");
    assert_eq!(content["encoding"], "utf8");
    assert_eq!(content["content"], text);
    assert!(content.get("reason").is_none());

    // Bytes that are not text come back base64.
    fixture.write("image.bin", &[0xff, 0xfe, 0x00, 0x01]);
    let (_, binary) = fixture
        .call(
            &token,
            "create",
            json!({ "path": "image.bin", "title": "Image", "mime": "image/png" }),
        )
        .await;
    let (_, content) = fixture
        .call(
            &token,
            "read",
            json!({ "version": binary["version"]["id"] }),
        )
        .await;
    assert_eq!(content["encoding"], "base64");
    assert_eq!(content["content"], "//4AAQ==");
    assert_eq!(content["mime"], "image/png");

    // Above the bound: the size and a reason, never the bytes.
    let big = vec![b'x'; READ_BOUND_BYTES as usize + 1];
    fixture.write("big.txt", &big);
    let (is_error, large) = fixture
        .call(
            &token,
            "create",
            json!({ "path": "big.txt", "title": "Big" }),
        )
        .await;
    assert!(!is_error, "{large}");
    let (is_error, content) = fixture
        .call(&token, "read", json!({ "version": large["version"]["id"] }))
        .await;
    assert!(!is_error, "{content}");
    assert_eq!(content["size_bytes"], READ_BOUND_BYTES + 1);
    assert_eq!(content["reason"], "content_too_large");
    assert_eq!(content["bound_bytes"], READ_BOUND_BYTES);
    assert!(content.get("content").is_none());
    assert!(content.get("encoding").is_none());

    // The hook: what the run registered, oldest first.
    let (status, produced) = json(
        &fixture.state,
        &fixture.bearer,
        "GET",
        &format!("/v1/artifacts/runs/{}/versions", fixture.run.to_storage()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<_> = produced["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            version.as_str(),
            binary["version"]["id"].as_str().unwrap(),
            large["version"]["id"].as_str().unwrap()
        ]
    );
    // And the person's own routes see the families in their workspace,
    // with the run as their creator.
    let (status, listed) = json(
        &fixture.state,
        &fixture.bearer,
        "GET",
        &format!("/v1/artifacts/families?workspace={}", fixture.workspace),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["families"].as_array().unwrap().len(), 3);
    assert_eq!(
        listed["families"][0]["created_by_run"],
        fixture.run.to_storage()
    );
}

#[tokio::test]
async fn create_version_attach_and_the_grant_set_over_a_persons_family() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace.clone();
    let (family, v1) = family_by_hand(&fixture.state, &fixture.bearer, &workspace).await;
    let token = fixture.token(&[&workspace]).await;
    fixture.write("report.md", b"second");

    let (is_error, attached) = fixture
        .call(&token, "attach_to_run", json!({ "version": v1 }))
        .await;
    assert!(!is_error, "{attached}");
    assert_eq!(attached["link"]["role"], "input");
    assert_eq!(attached["link"]["run"], fixture.run.to_storage());

    let (is_error, v2) = fixture
        .call(
            &token,
            "create_version",
            json!({ "family": family, "path": "report.md" }),
        )
        .await;
    assert!(!is_error, "{v2}");
    assert_eq!(v2["version"]["ordinal"], 2);
    assert_eq!(v2["version"]["derived_from"], v1);
    assert_eq!(
        v2["version"]["mime"], "text/plain",
        "the family's latest type"
    );
    assert_eq!(
        v2["version"]["produced_by"]["run"],
        fixture.run.to_storage()
    );
    let (status, provenance) = json(
        &fixture.state,
        &fixture.bearer,
        "GET",
        &format!(
            "/v1/artifacts/versions/{}/provenance",
            v2["version"]["id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let roles: Vec<_> = provenance["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|link| link["role"].as_str().unwrap().to_owned())
        .collect();
    assert!(roles.contains(&"output".to_owned()) && roles.contains(&"input".to_owned()));
}

// ---------------------------------------------------------- the refusals

/// Another workspace: a family the run was not granted, through every tool.
#[tokio::test]
async fn another_workspace_is_permission_denied_through_every_tool() {
    let fixture = Fixture::new().await;
    let elsewhere = make_workspace(&fixture.state, &fixture.bearer, "Elsewhere").await;
    let (family, version) = family_by_hand(&fixture.state, &fixture.bearer, &elsewhere).await;
    let token = fixture.token(&[&fixture.workspace.clone()]).await;
    fixture.write("x.md", b"x");
    assert_eq!(
        fixture
            .refused(&token, "get", json!({ "version": version }))
            .await,
        "permission_denied"
    );
    assert_eq!(
        fixture
            .refused(&token, "read", json!({ "version": version }))
            .await,
        "permission_denied"
    );
    assert_eq!(
        fixture
            .refused(&token, "attach_to_run", json!({ "version": version }))
            .await,
        "permission_denied"
    );
    assert_eq!(
        fixture
            .refused(
                &token,
                "create_version",
                json!({ "family": family, "path": "x.md" })
            )
            .await,
        "permission_denied"
    );
    let (_, found) = fixture.call(&token, "search", json!({ "query": "" })).await;
    assert!(
        found["hits"].as_array().unwrap().is_empty(),
        "search sees only granted workspaces"
    );
}

/// An empty grant set: every workspace call refused, `create` still works.
#[tokio::test]
async fn an_empty_grant_set_denies_all_workspace_access_and_create_still_lands() {
    let fixture = Fixture::new().await;
    let (family, version) =
        family_by_hand(&fixture.state, &fixture.bearer, &fixture.workspace.clone()).await;
    let token = fixture.token(&[]).await;
    fixture.write("own.md", b"mine");
    assert_eq!(
        fixture
            .refused(&token, "get", json!({ "version": version }))
            .await,
        "permission_denied"
    );
    assert_eq!(
        fixture
            .refused(&token, "search", json!({ "query": "" }))
            .await,
        "permission_denied"
    );
    assert_eq!(
        fixture
            .refused(
                &token,
                "create_version",
                json!({ "family": family, "path": "own.md" })
            )
            .await,
        "permission_denied"
    );
    let (is_error, created) = fixture
        .call(
            &token,
            "create",
            json!({ "path": "own.md", "title": "Mine" }),
        )
        .await;
    assert!(!is_error, "{created}");
    assert_eq!(
        created["family"]["workspace"], fixture.workspace,
        "the run's own workspace"
    );
    // Its own result stays reachable, and a further version of it too.
    let (is_error, _) = fixture
        .call(
            &token,
            "get",
            json!({ "version": created["version"]["id"] }),
        )
        .await;
    assert!(!is_error);
    fixture.write("own.md", b"mine, again");
    let (is_error, v2) = fixture
        .call(
            &token,
            "create_version",
            json!({ "family": created["family"]["id"], "path": "own.md" }),
        )
        .await;
    assert!(!is_error, "{v2}");
    assert_eq!(v2["version"]["ordinal"], 2);
}

/// Another run: a token for run A at run B's endpoint, and run B's token
/// against a family run A made.
#[tokio::test]
async fn another_runs_token_and_another_runs_family_are_both_refused() {
    let fixture = Fixture::new().await;
    let token_a = fixture.token(&[]).await;
    fixture.write("a.md", b"a");
    let (_, made_by_a) = fixture
        .call(&token_a, "create", json!({ "path": "a.md", "title": "A" }))
        .await;

    let run_b = RunId::mint();
    let folder_b = koru_artifact_core::artifacts_folder(fixture.run_workspace.path(), run_b);
    std::fs::create_dir_all(&folder_b).unwrap();
    let token_b = fixture.issue(run_b, &[]).await["token"]
        .as_str()
        .unwrap()
        .to_owned();

    // Run A's token at run B's URL: not this endpoint's, and it learns
    // nothing.
    let (status, body) = rpc(&fixture.state, run_b, &token_a, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "authority_unknown");
    // Run B against run A's family: neither its own nor granted.
    std::fs::write(folder_b.join("b.md"), b"b").unwrap();
    let (status, response) = rpc(
        &fixture.state,
        run_b,
        &token_b,
        "tools/call",
        json!({ "name": "create_version", "arguments": { "family": made_by_a["family"]["id"], "path": "b.md" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (is_error, output) = tool_output(&response);
    assert!(is_error);
    assert_eq!(output["code"], "permission_denied");
}

/// Forged provenance: a body that names a run, an actor or a workspace is
/// refused as invalid, never read — the tools take none of them.
#[tokio::test]
async fn a_body_naming_a_run_or_an_actor_is_refused_not_ignored() {
    let fixture = Fixture::new().await;
    let token = fixture.token(&[]).await;
    fixture.write("f.md", b"f");
    for forged in [
        json!({ "path": "f.md", "title": "F", "run": RunId::mint().to_storage() }),
        json!({ "path": "f.md", "title": "F", "produced_by": { "run": RunId::mint().to_storage(), "generation": 9 } }),
        json!({ "path": "f.md", "title": "F", "actor": "local" }),
        json!({ "path": "f.md", "title": "F", "workspace": fixture.workspace }),
    ] {
        assert_eq!(
            fixture.refused(&token, "create", forged).await,
            "invalid_request"
        );
    }
    assert_eq!(
        fixture
            .refused(
                &token,
                "search",
                json!({ "query": "", "workspace_ids": [fixture.workspace] })
            )
            .await,
        "invalid_request"
    );
    assert_eq!(
        fixture
            .refused(&token, "get", json!({ "version": "not-an-id" }))
            .await,
        "invalid_request"
    );
    assert_eq!(
        fixture.refused(&token, "nonsense", json!({})).await,
        "unknown_tool"
    );
    // And what does land carries the authority's provenance, not the
    // request's.
    let (_, created) = fixture
        .call(&token, "create", json!({ "path": "f.md", "title": "F" }))
        .await;
    assert_eq!(
        created["version"]["produced_by"]["run"],
        fixture.run.to_storage()
    );
    assert_eq!(created["version"]["produced_by"]["generation"], 1);
}

/// The register path rule, through the tool: out of scope, traversal, a
/// symlink, a junction, a directory, a missing file, a missing folder.
#[tokio::test]
async fn the_register_path_rule_refuses_each_shape_with_its_own_code() {
    let fixture = Fixture::new().await;
    let token = fixture.token(&[]).await;
    fixture.write("inside.md", b"inside");
    // A file in the run workspace but outside `.artifacts/<run>/`.
    let outside = fixture.run_workspace.path().join("outside.md");
    std::fs::write(&outside, b"outside").unwrap();
    // A sibling run's folder.
    let sibling = koru_artifact_core::artifacts_folder(fixture.run_workspace.path(), RunId::mint());
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(sibling.join("theirs.md"), b"theirs").unwrap();

    let refused = |path: String| {
        let token = token.clone();
        let fixture = &fixture;
        async move {
            fixture
                .refused(&token, "create", json!({ "path": path, "title": "T" }))
                .await
        }
    };
    assert_eq!(
        refused(outside.to_str().unwrap().to_owned()).await,
        "path_outside_run_folder"
    );
    assert_eq!(
        refused(sibling.join("theirs.md").to_str().unwrap().to_owned()).await,
        "path_outside_run_folder"
    );
    assert_eq!(refused("../outside.md".into()).await, "path_traversal");
    assert_eq!(
        refused("sub/../../outside.md".into()).await,
        "path_traversal"
    );
    assert_eq!(refused("missing.md".into()).await, "file_not_found");
    std::fs::create_dir_all(fixture.folder().join("dir")).unwrap();
    assert_eq!(refused("dir".into()).await, "not_a_file");
    assert_eq!(refused(".".into()).await, "not_a_file");

    // A symlink to a file that is itself inside the folder: containment
    // alone would pass it; the link is refused as a link.
    match symlink_file(
        &fixture.folder().join("inside.md"),
        &fixture.folder().join("link.md"),
    ) {
        Ok(()) => assert_eq!(refused("link.md".into()).await, "path_symlink"),
        Err(error) => {
            eprintln!("symlink case skipped: the OS refused to create one ({error})")
        }
    }
    // A symlinked directory on the way.
    std::fs::create_dir_all(fixture.folder().join("real")).unwrap();
    std::fs::write(fixture.folder().join("real").join("f.md"), b"f").unwrap();
    match symlink_dir(
        &fixture.folder().join("real"),
        &fixture.folder().join("linked"),
    ) {
        Ok(()) => assert_eq!(refused("linked/f.md".into()).await, "path_symlink"),
        Err(error) => {
            eprintln!("symlinked directory case skipped: the OS refused to create one ({error})")
        }
    }
    // A junction on the way (Windows).
    match junction(
        &fixture.folder().join("real"),
        &fixture.folder().join("junction"),
    ) {
        Ok(()) => assert_eq!(refused("junction/f.md".into()).await, "path_junction"),
        Err(error) => eprintln!("junction case skipped: {error}"),
    }
    // The folder itself replaced by a junction elsewhere: refused before
    // any file under it is considered.
    let run_c = RunId::mint();
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("c.md"), b"c").unwrap();
    let token_c = fixture.issue(run_c, &[]).await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let folder_c = koru_artifact_core::artifacts_folder(fixture.run_workspace.path(), run_c);
    if junction(elsewhere.path(), &folder_c).is_ok() {
        let (_, response) = rpc(
            &fixture.state,
            run_c,
            &token_c,
            "tools/call",
            json!({ "name": "create", "arguments": { "path": "c.md", "title": "C" } }),
        )
        .await;
        assert_eq!(tool_output(&response).1["code"], "path_junction");
    }
    // No folder at all: the agent registered before writing anything.
    let run_d = RunId::mint();
    let token_d = fixture.issue(run_d, &[]).await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_, response) = rpc(
        &fixture.state,
        run_d,
        &token_d,
        "tools/call",
        json!({ "name": "create", "arguments": { "path": "d.md", "title": "D" } }),
    )
    .await;
    assert_eq!(tool_output(&response).1["code"], "run_folder_missing");
    // The messages name no path.
    let (_, output) = fixture
        .call(
            &token,
            "create",
            json!({ "path": outside.to_str().unwrap(), "title": "T" }),
        )
        .await;
    let rendered = output.to_string();
    assert!(
        !rendered.contains("outside.md")
            && !rendered.contains(fixture.run_workspace.path().to_str().unwrap()),
        "{rendered}"
    );
}

fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link)
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link)
    }
}

fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link)
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link)
    }
}

fn junction(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (target, link);
        Err(std::io::Error::other("junctions exist only on Windows"))
    }
}

/// A file changed between the listing and the read: the seam inside the
/// rule, with the file rewritten at the same length and a moved
/// modification time, and with a different length.
#[test]
fn a_file_changed_between_listing_and_read_is_refused() {
    let run_workspace = tempfile::tempdir().unwrap();
    let run = RunId::mint();
    let folder = koru_artifact_core::artifacts_folder(run_workspace.path(), run);
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join("changing.md");
    std::fs::write(&file, b"first draft").unwrap();
    let authority = koru_artifact_core::RunAuthorityRecord {
        run,
        workspace: koru_artifact_core::WorkspaceId::from_uuid(uuid::Uuid::new_v4()),
        generation: AttemptGeneration::FIRST,
        grants: Default::default(),
        run_workspace: RunWorkspace::new(run_workspace.path()).unwrap(),
        state: koru_artifact_core::AuthorityState::Active,
        created_at_ms: 0,
        revoked_at_ms: None,
    };
    // Untouched: staged, with its canonical relative path and hash.
    let staged = staging::stage(&authority, "changing.md", || {}).unwrap();
    assert_eq!(staged.relative_path, "changing.md");
    assert_eq!(staged.bytes, b"first draft");
    assert_eq!(
        staged.hash.as_str(),
        format!("b3:{}", blake3::hash(b"first draft").to_hex())
    );
    assert!(!format!("{staged:?}").contains("changing"));
    // Same length, later modification time.
    let rewritten = {
        let file = file.clone();
        move || {
            std::fs::write(&file, b"FIRST DRAFT").unwrap();
            let handle = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
            handle
                .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(30))
                .unwrap();
        }
    };
    assert_eq!(
        staging::stage(&authority, "changing.md", rewritten).unwrap_err(),
        StageError::Changed
    );
    // A different length.
    let grown = {
        let file = file.clone();
        move || std::fs::write(&file, b"a much longer second draft").unwrap()
    };
    assert_eq!(
        staging::stage(&authority, "changing.md", grown).unwrap_err(),
        StageError::Changed
    );
    // Removed under the read.
    let removed = {
        let file = file.clone();
        move || std::fs::remove_file(&file).unwrap()
    };
    assert_eq!(
        staging::stage(&authority, "changing.md", removed).unwrap_err(),
        StageError::Unreadable
    );
    // Every code is spelled once and every message says something.
    let codes: Vec<_> = [
        StageError::RunFolderMissing,
        StageError::Traversal,
        StageError::Symlink,
        StageError::Junction,
        StageError::Outside,
        StageError::FileNotFound,
        StageError::NotAFile,
        StageError::Changed,
        StageError::Unreadable,
    ]
    .into_iter()
    .map(|error| {
        assert!(!error.message().is_empty());
        error.code()
    })
    .collect();
    let mut unique = codes.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), codes.len());
}

// ------------------------------------------------- generations and revocation

#[tokio::test]
async fn a_stale_generation_and_a_revoked_run_are_refused_by_token() {
    let fixture = Fixture::new().await;
    let first = fixture.token(&[]).await;
    fixture.write("r.md", b"r");
    let (is_error, _) = fixture
        .call(&first, "create", json!({ "path": "r.md", "title": "R" }))
        .await;
    assert!(!is_error);

    // The orchestrator resumes: a new generation, a new token, the same URL.
    let (status, advanced) = json(
        &fixture.state,
        &fixture.bearer,
        "POST",
        &format!(
            "/v1/artifacts/authorities/{}/generation",
            fixture.run.to_storage()
        ),
        Some(json!({ "generation": 2 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{advanced}");
    assert_eq!(advanced["generation"], 2);
    assert_eq!(
        advanced["url"],
        format!(
            "http://127.0.0.1:4242/v1/runs/{}/mcp",
            fixture.run.to_storage()
        )
    );
    let second = advanced["token"].as_str().unwrap().to_owned();
    assert_ne!(second, first);

    // The old token — a provider from the earlier attempt still running —
    // is refused with the fence's code, on every call, including
    // initialize.
    for method in ["initialize", "tools/list", "tools/call"] {
        let (status, body) = fixture
            .rpc(&first, method, json!({ "name": "get", "arguments": {} }))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method}");
        assert_eq!(body["code"], "stale_generation", "{method}");
    }
    // The new token works, and what it registers carries generation 2.
    fixture.write("r.md", b"r2");
    let (is_error, created) = fixture
        .call(&second, "create", json!({ "path": "r.md", "title": "R" }))
        .await;
    assert!(!is_error, "{created}");
    assert_eq!(created["version"]["produced_by"]["generation"], 2);
    assert_eq!(created["version"]["ordinal"], 2);
    // Not newer: refused at the route with the same code.
    let (status, body) = json(
        &fixture.state,
        &fixture.bearer,
        "POST",
        &format!(
            "/v1/artifacts/authorities/{}/generation",
            fixture.run.to_storage()
        ),
        Some(json!({ "generation": 2 })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "stale_generation");

    // Revoked: a state change, read by the next call.
    let (status, revoked) = json(
        &fixture.state,
        &fixture.bearer,
        "POST",
        &format!(
            "/v1/artifacts/authorities/{}/revoke",
            fixture.run.to_storage()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    assert_eq!(revoked["state"], "revoked");
    assert!(revoked.get("token").is_none());
    let (status, body) = fixture.rpc(&second, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "authority_revoked");
    let (status, body) = fixture.rpc(&first, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body["code"], "stale_generation",
        "a stale token learns nothing of the revocation"
    );
    // The record is readable, marked, without its token.
    let (status, view) = json(
        &fixture.state,
        &fixture.bearer,
        "GET",
        &format!("/v1/artifacts/authorities/{}", fixture.run.to_storage()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["state"], "revoked");
    assert_eq!(view["generation"], 2);
    assert!(view.get("token").is_none());
    assert!(view.get("run_workspace").is_none());
    // A second issue for a run that has an authority.
    let (status, body) = json(
        &fixture.state,
        &fixture.bearer,
        "POST",
        "/v1/artifacts/authorities",
        Some(json!({
            "run": fixture.run.to_storage(),
            "workspace": fixture.workspace,
            "generation": 1,
            "run_workspace": RunWorkspace::new(fixture.run_workspace.path()).unwrap(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "authority_exists");
}

/// The authority routes take the artifact scopes, refuse a relative
/// working scope, and name the published endpoint when the host set one.
#[tokio::test]
async fn the_authority_routes_take_the_artifact_scopes_and_name_the_published_endpoint() {
    let fixture = Fixture::new().await;
    let (read_only, _) =
        fixture
            .state
            .sessions
            .mint("reader", &["artifacts.read".into()], now_millis());
    let body = json!({
        "run": RunId::mint().to_storage(),
        "workspace": fixture.workspace,
        "generation": 1,
        "run_workspace": RunWorkspace::new(fixture.run_workspace.path()).unwrap(),
    });
    let (status, failure) = json(
        &fixture.state,
        &read_only,
        "POST",
        "/v1/artifacts/authorities",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(failure["code"], "forbidden");
    let (status, failure) = json(
        &fixture.state,
        &fixture.bearer,
        "POST",
        "/v1/artifacts/authorities",
        Some(json!({ "run": RunId::mint().to_storage(), "workspace": fixture.workspace, "generation": 1, "run_workspace": "relative/folder" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{failure}");
    assert_eq!(failure["code"], "invalid_request");
    let (status, failure) = json(
        &fixture.state,
        &fixture.bearer,
        "GET",
        &format!("/v1/artifacts/authorities/{}", RunId::mint().to_storage()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(failure["code"], "authority_not_found");

    assert!(
        fixture
            .state
            .set_public_endpoint("http://127.0.0.1:9999".into())
    );
    assert!(
        !fixture
            .state
            .set_public_endpoint("http://127.0.0.1:1".into())
    );
    let (status, issued) = json(
        &fixture.state,
        &fixture.bearer,
        "POST",
        "/v1/artifacts/authorities",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{issued}");
    assert_eq!(
        issued["url"],
        format!(
            "http://127.0.0.1:9999/v1/runs/{}/mcp",
            body["run"].as_str().unwrap()
        )
    );
    // The OpenAPI document names the endpoint and its six tools.
    let (_, document) = json(
        &fixture.state,
        &fixture.bearer,
        "GET",
        "/v1/openapi.json",
        None,
    )
    .await;
    let operation = &document["paths"]["/v1/runs/{run}/mcp"]["post"];
    assert_eq!(operation["operationId"], "runs_mcp");
    assert_eq!(operation["x-mcp-tools"].as_array().unwrap().len(), 6);
    assert!(document["paths"]["/v1/artifacts/authorities"]["post"].is_object());
}

// ------------------------------------------------------------- the restarts

/// A surviving provider keeps publishing across a delegate restart: there
/// is no delegate worker in this test at all, and nothing here changes
/// between the two calls but time.
#[tokio::test]
async fn a_surviving_provider_keeps_publishing_with_no_delegate_worker_alive() {
    let fixture = Fixture::new().await;
    let token = fixture.token(&[]).await;
    fixture.write("one.md", b"one");
    let (is_error, first) = fixture
        .call(
            &token,
            "create",
            json!({ "path": "one.md", "title": "One" }),
        )
        .await;
    assert!(!is_error, "{first}");
    // Whatever happened to the delegate worker in between — a crash, a
    // restart, a resume that has not reached the provider yet — the same
    // token still opens the same endpoint, with no adapter refresh.
    fixture.write("two.md", b"two");
    let (is_error, second) = fixture
        .call(
            &token,
            "create",
            json!({ "path": "two.md", "title": "Two" }),
        )
        .await;
    assert!(!is_error, "{second}");
    assert_eq!(second["version"]["produced_by"]["generation"], 1);
    let (_, found) = fixture
        .call(&token, "get", json!({ "version": first["version"]["id"] }))
        .await;
    assert_eq!(found["version"]["id"], first["version"]["id"]);
}

/// A gateway restart reloads the authority: a second `GatewayState` over
/// the same root — a fresh Store worker, `artifacts.db` reopened — answers
/// the token the first one issued.
#[tokio::test]
async fn a_restarted_gateway_reloads_the_authority_from_its_database() {
    let root = tempfile::tempdir().unwrap();
    let run_workspace = tempfile::tempdir().unwrap();
    let run = RunId::mint();
    let folder = koru_artifact_core::artifacts_folder(run_workspace.path(), run);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("before.md"), b"before").unwrap();
    let (token, workspace, before) = {
        let state = gateway(root.path());
        let (bearer, _) = state.sessions.mint("test", &[], now_millis());
        let workspace = make_workspace(&state, &bearer, "Notes").await;
        let (status, issued) = json(
            &state,
            &bearer,
            "POST",
            "/v1/artifacts/authorities",
            Some(json!({
                "run": run.to_storage(),
                "workspace": workspace,
                "generation": 1,
                "grants": [workspace],
                "run_workspace": RunWorkspace::new(run_workspace.path()).unwrap(),
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{issued}");
        let token = issued["token"].as_str().unwrap().to_owned();
        let (status, response) = rpc(
            &state,
            run,
            &token,
            "tools/call",
            json!({ "name": "create", "arguments": { "path": "before.md", "title": "Before" } }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (is_error, before) = tool_output(&response);
        assert!(!is_error, "{before}");
        state.stop_workers();
        (token, workspace, before)
    };
    // The gateway is gone; the next one opens the same root.
    let state = gateway(root.path());
    let (status, response) = rpc(&state, run, &token, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 6);
    let (status, response) = rpc(
        &state,
        run,
        &token,
        "tools/call",
        json!({ "name": "get", "arguments": { "version": before["version"]["id"] } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (is_error, found) = tool_output(&response);
    assert!(!is_error, "{found}");
    assert_eq!(found["family"]["workspace"], workspace);
    let (status, response) = rpc(
        &state,
        run,
        &token,
        "tools/call",
        json!({ "name": "search", "arguments": { "query": "before" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        tool_output(&response).1["hits"].as_array().unwrap().len(),
        1
    );
    // And a second root knows nothing of it.
    let other = tempfile::tempdir().unwrap();
    let elsewhere = gateway(other.path());
    let (status, body) = rpc(&elsewhere, run, &token, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "authority_unknown");
    elsewhere.stop_workers();
    state.stop_workers();
}
