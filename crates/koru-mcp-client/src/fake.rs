// SPDX-License-Identifier: MPL-2.0

//! A hosted MCP server in memory.
//!
//! **What is real and what is scripted.** The tools are real: every
//! `tools/call` runs `koru_mcp_server::WorkspaceToolExecutor`, the executor
//! the desktop bridge and the hosted server share, with the hosted rung's
//! capabilities, over `koru-mcp-server`'s in-memory Store. So the tool
//! schemas, the validation, the result shapes, `tool_not_available`, the
//! revision check on `base_revision`, the idempotency key and the three
//! outcome envelopes are the shared code's own, not this file's.
//!
//! Everything the hosted server adds around that executor is **written here
//! by hand from a document, and has never met the hosted server**:
//!
//! | Scripted | From |
//! | --- | --- |
//! | Claims ([`claims`]) | `contracts/hosted-tools-v1.md` section 4 |
//! | Which submission saves ([`store`], [`Fate`]) | the same, sections 1 and 2 |
//! | `claim` on reads | the same, section 4, "Claims shown on reads" |
//! | `profile_id`, `profile_name`, `name_status` | the same, section 5 |
//! | `image_upload_unavailable` for the three image tools | the same, section 3, "Image uploads" |
//! | `search_coverage`, `task_index_incomplete` | the tools' published descriptions; the field names from HOST.md |
//! | Revoked, subscription ended, unavailable | the same contract, section 2; the order from HOST.md |
//! | The front door: `401`, `403`, the revision and header checks | HOST.md, "Routes" |
//!
//! HOST.md is `docs/cloud-platform/mcp/HOST.md`, the team's internal page on
//! the hosted server as built. The design asked for a fake scripted from the
//! two published documents alone; they do not describe the front door at
//! all, so that part is from HOST.md and is marked where it is used.
//!
//! **No network and no disk.** A [`FakeConnection`] is the client's
//! [`Transport`] and its [`AccessTokens`]; a request is a function call.

mod claims;
mod store;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use koru_mcp_server::hosted::{CLAIM_DOCUMENT, RELEASE_DOCUMENT};
use koru_mcp_server::test_support::MemoryStore;
use koru_mcp_server::{
    Backend, DocumentClaims, HostCapabilities, LocalBackend, RUNG1_READ_TOOLS, RUNG1_TOOLS,
    RequestContext, ToolError, WorkspaceToolExecutor,
};
use koru_store_core::ids::DocumentId as StoreDocumentId;
use koru_store_core::model::Scope;
use rmcp::model::{CallToolResult, ErrorCode, ErrorData};
use serde_json::{Value, json};

use crate::{
    AccessToken, AccessTokens, Client, PostRequest, PostResponse, Refusal, Transport,
    TransportFailure,
};

use self::claims::Claims;
use self::store::ScriptedStore;

/// How one submission of a mutating call ends, when it does not simply save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fate {
    /// Koru Cloud refused it and it left nothing behind.
    NotSaved,
    /// It was submitted and never acknowledged, and it did not land.
    UnknownAndLost,
    /// It was submitted and never acknowledged, and it **did** land: the
    /// acknowledgement is what was lost.
    UnknownButLanded,
}

/// What a connection was approved for
/// (`docs/developers/APP-CONNECTIONS.md`, "The scope ceiling").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    ReadAndEdit,
}

/// The revisions HOST.md says the built server accepts.
const BUILT_SERVER_REVISIONS: [&str; 4] = ["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26"];

/// The fake's one Profile.
const PROFILE_NAME: &str = "Personal";

/// A hosted MCP server and the one workspace behind it.
#[derive(Clone)]
pub struct FakeCloud {
    shared: Arc<Shared>,
}

struct Shared {
    memory: Arc<MemoryStore>,
    store: Arc<ScriptedStore>,
    executor: WorkspaceToolExecutor,
    state: Arc<Mutex<State>>,
}

struct State {
    /// Unix milliseconds. Moved only by [`FakeCloud::advance_seconds`].
    now: i64,
    /// By the connection's name.
    connections: HashMap<String, Connection>,
    claims: Claims,
    subscription_ended: bool,
    authorization_unavailable: bool,
    revisions: Vec<String>,
    /// The documents each connection has opened: its replica's resident
    /// bodies (HOST.md, "Metadata, document bodies and search").
    resident: HashSet<(String, StoreDocumentId)>,
    names_loading: bool,
    tool_page: Option<usize>,
    event_stream: bool,
    profile: uuid::Uuid,
    /// Requests that reached the server.
    requests: usize,
    minted_tokens: u64,
}

struct Connection {
    access: Access,
    /// The one token the server accepts for this connection.
    token: String,
    revoked: bool,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Default for FakeCloud {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeCloud {
    pub fn new() -> Self {
        let memory = MemoryStore::new();
        let store = ScriptedStore::new(memory.clone());
        let state = Arc::new(Mutex::new(State {
            now: 1_800_000_000_000,
            connections: HashMap::new(),
            claims: Claims::default(),
            subscription_ended: false,
            authorization_unavailable: false,
            revisions: BUILT_SERVER_REVISIONS.map(str::to_owned).to_vec(),
            resident: HashSet::new(),
            names_loading: false,
            tool_page: None,
            event_stream: false,
            profile: uuid::Uuid::new_v4(),
            requests: 0,
            minted_tokens: 0,
        }));
        let backend = HostedBackend {
            local: LocalBackend::with_embedded(store.clone()),
            state: state.clone(),
        };
        Self {
            shared: Arc::new(Shared {
                memory,
                store,
                executor: WorkspaceToolExecutor::new(Arc::new(backend)),
                state,
            }),
        }
    }

    /// The one workspace every connection of this fake may reach.
    pub fn workspace_id(&self) -> String {
        self.shared.memory.workspace().to_storage()
    }

    /// A page already in Koru Cloud, with one line of text. No connection
    /// has opened it.
    pub fn add_page(&self, title: &str, first_line: &str) -> String {
        let document = self
            .shared
            .memory
            .add_document(store::KIND_PAGE, title, first_line);
        self.shared.store.index_line(document, first_line);
        document.to_storage()
    }

    /// A new connection, as one approval makes one
    /// (`APP-CONNECTIONS.md`: "Each approval is its own connection").
    pub fn connect(&self, name: &str, access: Access) -> Arc<FakeConnection> {
        let mut state = lock(&self.shared.state);
        state.minted_tokens += 1;
        let token = format!("fake-token-{}", state.minted_tokens);
        state.connections.insert(
            name.to_owned(),
            Connection {
                access,
                token: token.clone(),
                revoked: false,
            },
        );
        Arc::new(FakeConnection {
            cloud: self.clone(),
            name: name.to_owned(),
            held_token: Mutex::new(token),
            fault: Mutex::new(None),
        })
    }

    /// The account's Cloud subscription ends, or resumes.
    pub fn set_subscription_ended(&self, ended: bool) {
        lock(&self.shared.state).subscription_ended = ended;
    }

    /// The server cannot read connection state, or can again.
    pub fn set_authorization_unavailable(&self, unavailable: bool) {
        lock(&self.shared.state).authorization_unavailable = unavailable;
    }

    /// The revisions the server speaks. HOST.md's four by default.
    pub fn speak_only(&self, revisions: &[&str]) {
        lock(&self.shared.state).revisions = revisions.iter().map(|&r| r.to_owned()).collect();
    }

    /// Move the server's clock, which is what expires a claim.
    pub fn advance_seconds(&self, seconds: i64) {
        lock(&self.shared.state).now += seconds * 1_000;
    }

    /// Script the next mutating call: its submission number `submission`
    /// (counted from 0, in the order `contracts/hosted-tools-v1.md` section 1
    /// lists a tool's effects) ends in `fate`; the ones before it save.
    pub fn arm(&self, submission: usize, fate: Fate) {
        self.shared.store.arm(submission, fate);
    }

    /// Whether `list_workspaces` answers before the names have arrived
    /// (`name: null`, `name_status: "loading"`).
    pub fn set_names_loading(&self, loading: bool) {
        lock(&self.shared.state).names_loading = loading;
    }

    /// Serve `tools/list` this many tools at a time.
    pub fn page_tools(&self, size: usize) {
        lock(&self.shared.state).tool_page = Some(size.max(1));
    }

    /// Answer as an event stream of one message instead of plain JSON.
    pub fn answer_as_event_stream(&self, event_stream: bool) {
        lock(&self.shared.state).event_stream = event_stream;
    }

    /// How many requests have reached the server.
    pub fn requests(&self) -> usize {
        lock(&self.shared.state).requests
    }

    /// Whether the workspace's list has a page of this id.
    pub fn lists(&self, document_id: &str) -> bool {
        self.shared
            .store
            .listed_pages()
            .iter()
            .any(|entry| entry.id.to_storage() == document_id)
    }
}

/// What a test can make a connection's transport do once.
enum Fault {
    NotSent,
    AnswerLost,
}

/// One app connection: the client's two ports.
pub struct FakeConnection {
    cloud: FakeCloud,
    name: String,
    /// The token the credential's owner holds, which the server may have
    /// stopped accepting.
    held_token: Mutex<String>,
    fault: Mutex<Option<Fault>>,
}

impl FakeConnection {
    /// A client over this connection.
    pub fn client(self: &Arc<Self>) -> Client {
        Client::new(self.clone(), self.clone())
    }

    /// The server stops accepting the token this connection holds.
    pub fn expire_token(&self) {
        let mut state = lock(&self.cloud.shared.state);
        state.minted_tokens += 1;
        let next = format!("fake-token-{}", state.minted_tokens);
        if let Some(connection) = state.connections.get_mut(&self.name) {
            connection.token = next;
        }
    }

    /// The credential's owner renews: this connection now holds the token
    /// the server accepts.
    pub fn renew_token(&self) {
        let state = lock(&self.cloud.shared.state);
        if let Some(connection) = state.connections.get(&self.name) {
            *self
                .held_token
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = connection.token.clone();
        }
    }

    /// The person revokes the connection. Its claims are released with it
    /// (`APP-CONNECTIONS.md`, "Document claims").
    pub fn revoke(&self) {
        let mut state = lock(&self.cloud.shared.state);
        if let Some(connection) = state.connections.get_mut(&self.name) {
            connection.revoked = true;
        }
        state.claims.release_all(&self.name);
    }

    /// The next request reaches the server and is served, and its answer
    /// never comes back.
    pub fn lose_next_answer(&self) {
        self.set_fault(Fault::AnswerLost);
    }

    /// The next request never leaves.
    pub fn fail_before_sending(&self) {
        self.set_fault(Fault::NotSent);
    }

    fn set_fault(&self, fault: Fault) {
        *self
            .fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fault);
    }
}

impl AccessTokens for FakeConnection {
    fn access_token(&self) -> Result<AccessToken, Refusal> {
        let token = self
            .held_token
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        AccessToken::new(token).map_err(|_| Refusal::TokenExpired)
    }
}

impl Transport for FakeConnection {
    fn post(&self, request: &PostRequest) -> Result<PostResponse, TransportFailure> {
        let fault = self
            .fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        match fault {
            Some(Fault::NotSent) => Err(TransportFailure::NotSent),
            Some(Fault::AnswerLost) => {
                self.cloud.shared.serve(request);
                Err(TransportFailure::Unconfirmed)
            }
            None => Ok(self.cloud.shared.serve(request)),
        }
    }
}

// ----------------------------------------------------------- the front door

/// A JSON-RPC error answer.
fn rpc_refusal(status: u16, id: &Value, error: ErrorData) -> PostResponse {
    let mut message = json!({ "jsonrpc": "2.0", "error": error });
    if !id.is_null() {
        message["id"] = id.clone();
    }
    PostResponse {
        status,
        content_type: Some("application/json".into()),
        www_authenticate: None,
        body: serde_json::to_vec(&message).unwrap_or_default(),
    }
}

fn header<'a>(headers: &'a [(&'static str, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Admission's refusal, when the connection may not be served now.
fn admission(state: &State, connection: &Connection) -> Option<(&'static str, &'static str)> {
    // HOST.md, "How a call runs", step 2: "A revoked connection, an ended
    // subscription or an API that cannot answer is refused".
    if state.authorization_unavailable {
        Some((
            "authorization_unavailable",
            "Koru could not check this connection. Try again shortly.",
        ))
    } else if connection.revoked {
        Some(("authorization_revoked", "This connection was revoked."))
    } else if state.subscription_ended {
        Some((
            "cloud_subscription_ended",
            "The Koru Cloud subscription for this account has ended.",
        ))
    } else {
        None
    }
}

impl Shared {
    /// Serve one request the way HOST.md describes the front door: "checks
    /// the body, verifies the token, then checks headers for that protocol
    /// revision", then admission, then the shared executor.
    fn serve(&self, request: &PostRequest) -> PostResponse {
        let headers = request.headers();
        let event_stream = {
            let mut state = lock(&self.state);
            state.requests += 1;
            state.event_stream
        };

        // The body.
        let Ok(body) = serde_json::from_slice::<Value>(request.body()) else {
            return rpc_refusal(
                400,
                &Value::Null,
                ErrorData::parse_error("The request is not JSON.", None),
            );
        };
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let (Some("2.0"), Some(method)) = (
            body.get("jsonrpc").and_then(Value::as_str),
            body.get("method").and_then(Value::as_str),
        ) else {
            return rpc_refusal(
                400,
                &id,
                ErrorData::invalid_request("The request is not a JSON-RPC request.", None),
            );
        };
        let params = body.get("params").cloned().unwrap_or(Value::Null);

        // The token. "A missing or invalid token is 401 with
        // `WWW-Authenticate: Bearer resource_metadata=…`". The address in the
        // challenge is the hosted server's own; the fake has none, and names
        // a host under the reserved `.invalid` domain.
        let presented =
            header(&headers, "Authorization").and_then(|value| value.strip_prefix("Bearer "));
        let (name, access) = {
            let state = lock(&self.state);
            let found = state
                .connections
                .iter()
                .find(|(_, connection)| Some(connection.token.as_str()) == presented)
                .map(|(name, connection)| (name.clone(), connection.access));
            match found {
                Some(found) => found,
                None => {
                    return PostResponse {
                        status: 401,
                        content_type: None,
                        www_authenticate: Some(
                            "Bearer resource_metadata=\"https://fake.invalid/.well-known/oauth-protected-resource/mcp\""
                                .into(),
                        ),
                        body: Vec::new(),
                    };
                }
            }
        };

        // The revision and its headers. "Modern requests still require
        // matching `MCP-Protocol-Version`, `Mcp-Method`, `Mcp-Name` and
        // version metadata. A mismatch is 400 `-32020`; an unsupported
        // explicit version header is 400 `-32022`, listing the supported
        // revisions."
        let mismatch =
            |what: &'static str| rpc_refusal(400, &id, ErrorData::header_mismatch(what, None));
        let stated = params
            .get("_meta")
            .and_then(|meta| meta.get("io.modelcontextprotocol/protocolVersion"))
            .and_then(Value::as_str);
        let Some(revision) = header(&headers, "MCP-Protocol-Version") else {
            return mismatch("MCP-Protocol-Version is required.");
        };
        let supported = lock(&self.state).revisions.clone();
        if !supported.iter().any(|known| known == revision) {
            return rpc_refusal(
                400,
                &id,
                ErrorData::new(
                    ErrorCode::UNSUPPORTED_PROTOCOL_VERSION,
                    "Unsupported protocol version",
                    Some(json!({ "requested": revision, "supported": supported })),
                ),
            );
        }
        if stated != Some(revision) {
            return mismatch("MCP-Protocol-Version differs from the request's own.");
        }
        if header(&headers, "Mcp-Method") != Some(method) {
            return mismatch("Mcp-Method differs from the request's own.");
        }
        let tool = match method {
            "tools/list" => None,
            "tools/call" => {
                let Some(tool) = params.get("name").and_then(Value::as_str) else {
                    return rpc_refusal(
                        400,
                        &id,
                        ErrorData::new(ErrorCode::INVALID_PARAMS, "tools/call names a tool.", None),
                    );
                };
                if header(&headers, "Mcp-Name") != Some(tool) {
                    return mismatch("Mcp-Name differs from the request's own.");
                }
                Some(tool.to_owned())
            }
            // "An unknown method is 404 `-32601`".
            _ => {
                return rpc_refusal(
                    404,
                    &id,
                    ErrorData::new(ErrorCode::METHOD_NOT_FOUND, "Method not found", None),
                );
            }
        };

        // Admission.
        let refused = {
            let state = lock(&self.state);
            state
                .connections
                .get(&name)
                .and_then(|connection| admission(&state, connection))
        };
        let capabilities = match access {
            Access::Read => HostCapabilities::rung1_reads(),
            Access::ReadAndEdit => HostCapabilities::rung1(),
        };

        let result = match (tool, refused) {
            // A tool call answers admission's refusal as a tool error: these
            // are among "the refusal's own code" of
            // `contracts/hosted-tools-v1.md` section 2.
            (Some(_), Some((code, message))) => tool_result(Err(ToolError {
                code: code.into(),
                message: message.into(),
                details: None,
            })),
            // **Not in any document:** how a refused `tools/list` travels.
            // The fake answers `403` and a JSON-RPC error naming the code in
            // its data.
            (None, Some((code, message))) => {
                return rpc_refusal(
                    403,
                    &id,
                    ErrorData::new(ErrorCode(-32000), message, Some(json!({ "code": code }))),
                );
            }
            (None, None) => self.catalog(capabilities, &params),
            (Some(tool), None) => {
                // "A Read connection calling an edit tool is 403
                // `insufficient_scope`."
                if access == Access::Read
                    && RUNG1_TOOLS.contains(&tool.as_str())
                    && !RUNG1_READ_TOOLS.contains(&tool.as_str())
                {
                    let mut refusal = rpc_refusal(
                        403,
                        &id,
                        ErrorData::new(
                            ErrorCode(-32000),
                            "This connection may read only.",
                            Some(json!({ "code": "insufficient_scope" })),
                        ),
                    );
                    refusal.www_authenticate = Some("Bearer error=\"insufficient_scope\"".into());
                    return refusal;
                }
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                tool_result(self.call(&name, access, capabilities, &tool, arguments))
            }
        };

        let message = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let json = serde_json::to_vec(&message).unwrap_or_default();
        if event_stream {
            let mut body = b"event: message\ndata: ".to_vec();
            body.extend(json);
            body.extend(b"\n\n");
            PostResponse {
                status: 200,
                content_type: Some("text/event-stream".into()),
                www_authenticate: None,
                body,
            }
        } else {
            PostResponse {
                status: 200,
                content_type: Some("application/json".into()),
                www_authenticate: None,
                body: json,
            }
        }
    }

    /// `tools/list`: the catalog the shared crate publishes for this rung.
    fn catalog(&self, capabilities: HostCapabilities, params: &Value) -> Value {
        let tools: Vec<Value> = capabilities
            .catalog()
            .into_iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "title": tool.title,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                    "annotations": {
                        "readOnlyHint": tool.safety.read_only,
                        "destructiveHint": tool.safety.destructive,
                        "idempotentHint": tool.safety.idempotent,
                        "openWorldHint": tool.safety.open_world,
                    },
                })
            })
            .collect();
        let page = lock(&self.state).tool_page.unwrap_or(tools.len().max(1));
        let start = params
            .get("cursor")
            .and_then(Value::as_str)
            .and_then(|cursor| cursor.parse::<usize>().ok())
            .unwrap_or(0)
            .min(tools.len());
        let end = (start + page).min(tools.len());
        let mut result = json!({ "tools": tools[start..end], "resultType": "complete" });
        if end < tools.len() {
            result["nextCursor"] = json!(end.to_string());
        }
        result
    }

    /// One tool call: the shared executor, and around it what the hosted
    /// server adds.
    fn call(
        &self,
        connection: &str,
        access: Access,
        capabilities: HostCapabilities,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, ToolError> {
        let scopes = match access {
            Access::Read => vec![Scope::ReadDocuments],
            Access::ReadAndEdit => vec![Scope::ReadDocuments, Scope::EditDocuments],
        };
        let context =
            RequestContext::selected("fake", connection, scopes, [self.memory.workspace()])
                .with_capabilities(capabilities)
                .with_document_claims(Arc::new(ClaimGate {
                    state: self.state.clone(),
                    connection: connection.to_owned(),
                }));

        let eligible = self.store.listed_pages();
        let resident: HashSet<StoreDocumentId> = {
            let state = lock(&self.state);
            eligible
                .iter()
                .map(|entry| entry.id)
                .filter(|id| state.resident.contains(&(connection.to_owned(), *id)))
                .collect()
        };

        // "Hosted Cloud MCP returns task_index_incomplete when document
        // bodies are unopened" (`list_tasks`' published description), "with
        // the same counts" as a search's coverage (HOST.md).
        if tool == "list_tasks" && resident.len() < eligible.len() {
            return Err(ToolError {
                code: "task_index_incomplete".into(),
                message:
                    "Some documents have not been opened, so the task list would be incomplete."
                        .into(),
                details: Some(json!({
                    "content_documents_searched": resident.len(),
                    "eligible_documents": eligible.len(),
                    "partial": true,
                })),
            });
        }

        self.store.begin_call(resident.clone());
        let answered = self.executor.invoke(&context, tool, arguments.clone());
        self.store.end_call();
        let mut answer = answered?;

        let mut state = lock(&self.state);
        let now = state.now;
        let opened = |state: &mut State, id: Option<&str>| {
            if let Some(document) = id.and_then(StoreDocumentId::parse) {
                state.resident.insert((connection.to_owned(), document));
            }
        };
        let argument = |key: &str| arguments.get(key).and_then(Value::as_str);
        match tool {
            // "The hosted `read_document` result, and each entry of the
            // hosted `list_documents` result, add `claim`".
            "read_document" => {
                opened(&mut state, argument("document_id"));
                if let Some(claim) =
                    argument("document_id").and_then(|id| state.claims.shown(now, id))
                {
                    answer["claim"] = claim;
                }
            }
            "list_documents" => {
                if let Some(documents) = answer.get_mut("documents").and_then(Value::as_array_mut) {
                    for entry in documents {
                        let claim = entry
                            .get("id")
                            .and_then(Value::as_str)
                            .and_then(|id| state.claims.shown(now, id));
                        if let Some(claim) = claim {
                            entry["claim"] = claim;
                        }
                    }
                }
            }
            // "search_coverage reports the counts" (`search_documents`'
            // published description); the names are HOST.md's.
            "search_documents" => {
                answer["search_coverage"] = json!({
                    "content_documents_searched": resident.len(),
                    "eligible_documents": eligible.len(),
                    "partial": resident.len() < eligible.len(),
                });
            }
            // Section 5: "`list_workspaces` entry `name`, `profile_id`,
            // `profile_name`, `name_status`".
            "list_workspaces" => {
                let (profile, loading) = (state.profile, state.names_loading);
                if let Some(workspaces) = answer.get_mut("workspaces").and_then(Value::as_array_mut)
                {
                    for entry in workspaces {
                        entry["profile_id"] = json!(profile.to_string());
                        entry["profile_name"] = json!(PROFILE_NAME);
                        if loading {
                            entry["name"] = Value::Null;
                            entry["name_status"] = json!("loading");
                        }
                    }
                }
            }
            // "Documents created by a tool enter the resident set when their
            // creation succeeds" (HOST.md), and an edit fetches the document
            // it names.
            "create_document" => {
                let created = answer
                    .get("document_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                opened(&mut state, created.as_deref());
            }
            "edit_document" => opened(&mut state, argument("document_id")),
            "extract_to_document" => {
                opened(&mut state, argument("source_document_id"));
                let destination = answer
                    .get("document_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                opened(&mut state, destination.as_deref());
            }
            _ => {}
        }
        Ok(answer)
    }
}

/// A tool's answer as the server sends it: the constructors the shared
/// server itself uses, so the shape is the protocol library's.
fn tool_result(answer: Result<Value, ToolError>) -> Value {
    let result = match answer {
        Ok(value @ Value::Object(_)) => CallToolResult::structured(value),
        Ok(value) => CallToolResult::structured(json!({ "result": value })),
        Err(error) => {
            let mut object = json!({ "code": error.code, "message": error.message });
            if let Some(details) = error.details {
                object["details"] = details;
            }
            CallToolResult::structured_error(object)
        }
    };
    serde_json::to_value(result).unwrap_or(Value::Null)
}

// -------------------------------------------- what the hosted server adds

/// The backend the executor calls: the shared one, with the two claim tools
/// and the claim check in front of it.
struct HostedBackend {
    local: LocalBackend,
    state: Arc<Mutex<State>>,
}

impl Backend for HostedBackend {
    fn invoke(
        &self,
        context: &RequestContext,
        tool: &str,
        mut arguments: Value,
    ) -> Result<Value, ToolError> {
        let connection = context.principal.as_str();
        match tool {
            CLAIM_DOCUMENT => {
                let mut state = lock(&self.state);
                let now = state.now;
                state.claims.take(now, connection, &arguments)
            }
            RELEASE_DOCUMENT => lock(&self.state).claims.release(connection, &arguments),
            // "The documents a call changes are its targets:
            // `edit_document`'s document; ... `extract_to_document`'s source
            // and, when it extracts into one, its existing destination. A new
            // document is never a target."
            "edit_document" | "extract_to_document" => {
                let claim_ids = claims::take_claim_ids(&mut arguments)?;
                let targets: &[&str] = if tool == "edit_document" {
                    &["document_id"]
                } else {
                    &["source_document_id", "target_document_id"]
                };
                {
                    let state = lock(&self.state);
                    for target in targets {
                        if let Some(document) = arguments.get(*target).and_then(Value::as_str) {
                            state
                                .claims
                                .check(state.now, connection, document, &claim_ids)?;
                        }
                    }
                }
                self.local.invoke(context, tool, arguments)
            }
            // The executor has already asked the gate below.
            "add_image" | "replace_image" => {
                claims::take_claim_ids(&mut arguments)?;
                self.local.invoke(context, tool, arguments)
            }
            _ => self.local.invoke(context, tool, arguments),
        }
    }

    fn doctor(&self) -> Result<Value, ToolError> {
        Ok(json!({ "ok": true }))
    }
}

/// The check the executor runs for the image tools, which is the one an edit
/// gets.
struct ClaimGate {
    state: Arc<Mutex<State>>,
    connection: String,
}

impl DocumentClaims for ClaimGate {
    fn enforce(
        &self,
        _workspace_id: &str,
        document_id: &str,
        claim_ids: &[String],
    ) -> Result<(), ToolError> {
        let state = lock(&self.state);
        state
            .claims
            .check(state.now, &self.connection, document_id, claim_ids)
    }
}

#[cfg(test)]
mod tests;
